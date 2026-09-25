//! Validated committed-prefix scans of one segment file.
//!
//! Both the S2 verification read and the S4 query surface need exactly the
//! same guarantee — the bytes the visibility transaction published, with
//! every structural invariant checked — so the scan lives here once and
//! the callers differ only in what they do with the frames.

use std::path::Path;

use crate::db::SegmentRow;
use crate::error::StorageError;
use crate::frame::{SEGMENT_HEADER_LEN, ScanOutcome, ScanReport, SegmentHeader, scan_frames};
use crate::identity::HeaderIdentity;
use crate::recovery::frames_match_row;

/// One segment file whose committed prefix validated cleanly, with the
/// data its frame offsets index into.
pub(crate) struct ScannedSegment {
    /// The whole file; frame offsets index into it (only the committed
    /// prefix is scanned, so offsets never reach the tail).
    pub data: Vec<u8>,
    /// The validated frame sequence of the committed prefix.
    pub report: ScanReport,
}

/// Read one segment file and validate its committed prefix. Errors (never
/// silent narrowing) when the file is shorter than the committed boundary,
/// its segment header does not match the row's identity and kind (a file
/// the row never owned — e.g. an old-epoch file left behind by a rebuild
/// without a prior recovery), the committed prefix is not a clean frame
/// sequence of the segment's kind, or the frames' coordinates do not agree
/// with the row's indexed range.
pub(crate) async fn scan_committed(
    segment: &SegmentRow,
    root: &Path,
    identity: &HeaderIdentity,
) -> Result<ScannedSegment, StorageError> {
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
    let report = scan_frames(
        &data[..committed],
        SEGMENT_HEADER_LEN,
        segment.kind.segment_kind(),
    );
    if report.outcome != ScanOutcome::Clean || !frames_match_row(&report, segment) {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "segment {} committed prefix is not clean or does not match its \
                 indexed range; run recovery before reading",
                segment.file_name
            ),
        });
    }
    Ok(ScannedSegment { data, report })
}
