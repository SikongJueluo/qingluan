//! Committed-prefix verification reads of the terminal log.
//!
//! The fixed-range read / tail / grep query API belongs to S4 and is
//! intentionally absent from this crate's public seam. What S2 must prove
//! at the storage seam is kept here, under `test-hooks`: the
//! committed-prefix scan. It returns exactly the bytes the visibility
//! transaction published for one stream — never fsynced-but-uncommitted
//! tails — and rejects (instead of silently narrowing) any segment whose
//! committed prefix is not a clean frame sequence. Reads are
//! stream-typed: a raw read only ever touches `raw` segments and a
//! normalized read only `normalized` segments, so the two streams can
//! never be mixed into one result or cross-addressed.

use std::path::Path;

use qingluan_core::terminal::LogIdentity;

use crate::db::SegmentRow;
use crate::error::StorageError;
use crate::frame::{SEGMENT_HEADER_LEN, ScanOutcome, SegmentHeader, scan_frames};
use crate::identity::{HeaderIdentity, ResolvedIdentity};
use crate::recovery::{ChainLink, frames_match_row, validate_chain};
use crate::{LogStore, LogStream};

/// Read one segment file and validate its committed prefix. Errors (never
/// silent narrowing) when the file is shorter than the committed boundary,
/// its segment header does not match the row's identity and kind (a file
/// the row never owned — e.g. an old-epoch file left behind by a rebuild
/// without a prior recovery), the committed prefix is not a clean frame
/// sequence of the segment's kind, or the frames' coordinates do not agree
/// with the row's indexed range.
async fn scan_committed(
    segment: &SegmentRow,
    root: &Path,
    identity: &HeaderIdentity,
) -> Result<Vec<u8>, StorageError> {
    let path = crate::paths::segment_path(root, &segment.file_name);
    let data = tokio::fs::read(&path)
        .await
        .map_err(|source| StorageError::Io { path, source })?;
    let header = SegmentHeader::parse(&data).map_err(|error| StorageError::RecoveryRequired {
        detail: format!(
            "segment {} header is invalid ({error}); run recovery before reading",
            segment.file_name
        ),
    })?;
    // The file must be exactly what the row claims: kind, terminal,
    // epoch, and segment id. Otherwise the row never owned these bytes
    // and they must not be returned as this log's committed prefix.
    if header.kind != segment.kind.segment_kind()
        || header.terminal != identity.terminal_uuid
        || header.epoch != identity.epoch
        || header.segment_id != segment.segment_id as u64
    {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "segment {} header does not match its row or this log's identity; \
                 run recovery before reading",
                segment.file_name
            ),
        });
    }
    if (data.len() as u64) < segment.committed_bytes {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "segment {} is {} bytes on disk but its committed boundary is {}; \
                 run recovery before reading",
                segment.file_name,
                data.len(),
                segment.committed_bytes
            ),
        });
    }
    let committed = segment.committed_bytes as usize;
    let scan = scan_frames(
        &data[..committed],
        SEGMENT_HEADER_LEN,
        segment.kind.segment_kind(),
    );
    if scan.outcome != ScanOutcome::Clean || !frames_match_row(&scan, segment) {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "segment {} committed prefix is not clean or does not match its \
                 indexed range; run recovery before reading",
                segment.file_name
            ),
        });
    }
    Ok(data)
}

impl LogStore {
    /// Concatenate the payloads of every committed frame of one stream,
    /// oldest segment first — byte-exact, NUL- and invalid-UTF-8-safe for
    /// the raw stream. Verification surface for the S2 commit-order and
    /// visibility proofs only; the query read API arrives in S4.
    #[doc(hidden)]
    pub async fn read_committed(
        &self,
        log: &LogIdentity,
        stream: LogStream,
    ) -> Result<Vec<u8>, StorageError> {
        let ident = ResolvedIdentity::parse(log)?;
        let term = self
            .store
            .terminal(&ident.key)
            .await?
            .ok_or_else(|| StorageError::UnknownLog(ident.terminal_id.clone()))?;
        if term.log_epoch != ident.header.epoch {
            return Err(StorageError::EpochMismatch {
                stored: uuid::Uuid::from_bytes(term.log_epoch).to_string(),
                requested: ident.epoch,
            });
        }
        let mut out = Vec::new();
        // `segments` filters by kind: a read of one stream can never
        // traverse the other stream's segments.
        let segments = self.store.segments(&ident.key, stream).await?;
        for segment in &segments {
            if segment.state != "active" && segment.state != "sealed" {
                continue; // quarantined/missing: recovery-owned, invisible here
            }
            let data = scan_committed(segment, &self.root, &ident.header).await?;
            let committed = segment.committed_bytes as usize;
            let scan = scan_frames(
                &data[..committed],
                SEGMENT_HEADER_LEN,
                segment.kind.segment_kind(),
            );
            for frame in scan.frames {
                out.extend_from_slice(&data[frame.payload_at..frame.end - 4]);
            }
        }
        // Every surviving segment validated alone; now the ordered
        // cross-segment chain must hold as a whole — strictly forward from
        // the retained floor to the watermark, with every hole covered by
        // an explicit gap record. A violation (an overlap or an uncovered
        // hole, e.g. after direct database tampering) refuses the read
        // until recovery ran; it is never silently narrowed away.
        let mut links: Vec<ChainLink> = segments
            .iter()
            .filter(|segment| {
                (segment.state == "active" || segment.state == "sealed")
                    && segment.stream_range().is_some()
            })
            .map(|segment| {
                let (start, end) = segment.stream_range().expect("non-empty range");
                ChainLink {
                    segment_id: segment.segment_id,
                    start,
                    end,
                }
            })
            .collect();
        links.sort_by_key(|link| (link.start, link.segment_id));
        let (floor, watermark) = match stream {
            LogStream::Normalized => (term.retained_first_line, term.line_watermark),
            LogStream::Raw => (term.retained_first_offset, term.raw_watermark),
        };
        let gaps = self.store.gaps(&ident.key, stream).await?;
        validate_chain(stream, floor, watermark, &links, &gaps).map_err(|violation| {
            StorageError::RecoveryRequired {
                detail: violation.detail(stream),
            }
        })?;
        Ok(out)
    }
}
