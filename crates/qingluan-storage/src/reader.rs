//! Committed-prefix verification reads of the terminal log.
//!
//! The fixed-range read / tail / grep query API lives in [`crate::query`];
//! what S2 must prove at the storage seam is kept here, under
//! `test-hooks`: the committed-prefix scan. It returns exactly the bytes
//! the visibility transaction published for one stream — never
//! fsynced-but-uncommitted tails — and rejects (instead of silently
//! narrowing) any segment whose committed prefix is not a clean frame
//! sequence. Reads are stream-typed: a raw read only ever touches `raw`
//! segments and a normalized read only `normalized` segments, so the two
//! streams can never be mixed into one result or cross-addressed.
//!
//! Both the verification read and the query surface validate a segment
//! file through the same [`crate::scan::scan_committed`] pass, so a
//! segment can never be acceptable to one and rejected by the other.

use qingluan_core::terminal::LogIdentity;

use crate::error::StorageError;
use crate::identity::ResolvedIdentity;
use crate::recovery::{ChainLink, validate_chain};
use crate::scan::scan_committed;
use crate::{LogStore, LogStream};

impl LogStore {
    /// Concatenate the payloads of every committed frame of one stream,
    /// oldest segment first — byte-exact, NUL- and invalid-UTF-8-safe for
    /// the raw stream. Verification surface for the S2 commit-order and
    /// visibility proofs only; the S4 query read is [`crate::LogStore::read`].
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
            let scan = scan_committed(segment, &self.root, &ident.header).await?;
            let data = scan.data;
            for frame in scan.report.frames {
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
