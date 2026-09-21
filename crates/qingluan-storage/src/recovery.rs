//! The S2 recovery pass: one idempotent, durability-ordered repair of a
//! terminal's log state, for both stream kinds.
//!
//! Recovery never fabricates continuity and never reuses a number. For
//! every segment row of the terminal it decides between exactly three
//! outcomes: the committed prefix validates — clean frames whose first
//! and last coordinates match the row's indexed range, with at most an
//! uncommitted tail beyond it — the row is tombstoned whole (zero
//! committed bytes: a crashed creation, never adopted; its file is
//! quarantined whole so the pending bytes survive as an artifact), or
//! the backing data is missing/truncated/corrupt/row-inconsistent (an
//! explicit stream-scoped `log_gap` is recorded, the terminal latches
//! `degraded`, the row is deleted, and the file is quarantined whole).
//! Watermarks are never written by recovery: they never decrease and
//! their numbers are never reused.
//!
//! Loss atomicity: recording one irrecoverable segment — the coalesced
//! gap, the active-pointer clear, the row tombstone, and the `degraded`
//! latch — is **one** SQLite transaction, so a crash can never leave a
//! half-recorded loss (a gap without its tombstone, a lost range without
//! the latch). The file rename that follows is outside the transaction
//! on purpose: a crash in between leaves an unclaimed file, which the
//! rerun's orphan pass quarantines whole.
//!
//! Chain validation: after the per-row repairs, the surviving rows of
//! each stream must form one ordered, non-overlapping chain from the
//! retained floor to the watermark, and every hole in it must be covered
//! by an explicit gap record. An overlap is fabricated continuity (the
//! row is lost as corrupt); an uncovered hole is an unrecorded loss (the
//! hole is recorded as a gap and the terminal latches `degraded`). The
//! chain is validated *against* the watermark, never past it: a row (or
//! the retained floor itself) extending beyond the watermark-derived
//! expected end claims positions the watermark never consumed — the row
//! is lost as corrupt, and a floor beyond the end is clamped back under
//! it. Both repairs are idempotent, and each strictly shrinks the work
//! left, so the pass converges. Reads run the same validation and refuse
//! (never silently narrowing) until recovery has run.
//!
//! Ownership: the pass holds the log's exclusive writer lease for its
//! whole duration — it must not repair state underneath a live writer,
//! and a live writer must not append underneath a repairing pass.
//!
//! Durability ordering (every step individually idempotent, so an
//! interruption between any two steps converges on the rerun):
//!
//! * an orphan segment file (no row claims it; after a destructive
//!   database rebuild these are the old epoch's files) is quarantined
//!   whole by rename, then the directory is fsynced;
//! * an uncommitted tail is first persisted as a quarantine artifact
//!   (written, `sync_data`d, directory fsynced) and only then is the live
//!   file truncated back to its committed boundary and synced;
//! * an irrecoverable segment records its gap, clears its pointer,
//!   deletes its row and latches `degraded` in one committed transaction
//!   *before* its file is renamed to quarantine, so a crash in between
//!   leaves a recorded loss, never an unrecorded loss.
//!
//! Filesystem errors of every repair step — file sync, the rename source
//! and destination directory sync, the artifact sync — propagate as
//! typed [`StorageError::Io`]; none is swallowed. A second run of a
//! completed recovery takes zero actions and observes an equal durable
//! state.

use std::collections::HashSet;
use std::path::Path;

use qingluan_core::terminal::LogIdentity;
use tokio::io::AsyncWriteExt;

use crate::LogStream;
use crate::db::{SegmentRow, Store};
use crate::error::{StorageError, io_error};
use crate::frame::{
    FRAME_FLAG_LINE_END, SEGMENT_HEADER_LEN, ScanOutcome, ScanReport, SegmentHeader, scan_frames,
};
use crate::gap::{GapReason, GapSpan};
use crate::identity::ResolvedIdentity;
use crate::lease::WriterLease;
use crate::paths;

/// One explicit gap of one stream as the production recovery seam
/// reports it: domain coordinates only. The stream discriminates the
/// coordinate system (normalized: 1-based line numbers; raw: stream byte
/// offsets); no segment id, file name, quarantine artifact, or
/// truncation boundary ever crosses the public seam — those are storage
/// internals kept under `test-hooks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryGap {
    /// The stream the gap belongs to (ranges never merge across
    /// streams).
    pub stream: LogStream,
    /// First missing line (1-based) or byte offset.
    pub start: u64,
    /// One past the last missing line or byte offset.
    pub end: u64,
    /// Why the range was declared missing.
    pub reason: GapReason,
}

/// What the recovery pass did, at domain level: whether it repaired
/// anything, whether the terminal is latched `degraded` (explicit losses
/// exist), and the coalesced explicit gap state of both streams after
/// the pass. The detailed per-repair actions (segment ids, file names,
/// quarantine artifacts, truncation boundaries) are storage internals
/// exposed only under `test-hooks` for the durability proofs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Whether the pass performed any repair. A completed pass rerun
    /// reports `false`.
    pub repaired: bool,
    /// Whether the terminal is latched `degraded` after the pass (the
    /// latch is permanent until a destructive rebuild).
    pub degraded: bool,
    /// The coalesced explicit gap state after the pass: normalized gaps
    /// first, then raw gaps, each oldest first.
    pub gaps: Vec<RecoveryGap>,
    /// Every repair performed, in execution order (test builds only:
    /// verification surface for the durability and idempotence proofs).
    #[cfg(any(test, feature = "test-hooks"))]
    pub actions: Vec<RecoveryAction>,
}

impl RecoveryReport {
    /// Whether the pass performed no repair.
    pub fn is_empty(&self) -> bool {
        !self.repaired
    }
}

/// One repair the recovery pass performed (crate-private: the production
/// seam reports the domain-level [`RecoveryReport`]; the durability
/// proofs read this through `test-hooks`). A second run of a completed
/// recovery produces an empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// A segment file with no row claiming it was quarantined whole by
    /// rename (orphan crash window, or an old epoch's file after a
    /// destructive database rebuild). Never adopted.
    OrphanSegmentQuarantined { file_name: String },
    /// A segment row with zero committed bytes was tombstoned (deleted)
    /// and its file, if any, was quarantined whole — never adopted, not
    /// even a header-valid pending file.
    ZeroByteRowTombstoned { segment_id: i64 },
    /// The uncommitted tail beyond a segment's committed boundary was
    /// persisted as a quarantine artifact first, then truncated from the
    /// live file (`truncated_to` is the committed boundary it returned
    /// to).
    TailQuarantined {
        file_name: String,
        quarantined_bytes: u64,
        truncated_to: u64,
    },
    /// An indexed segment was irrecoverable; its full stream range is now
    /// an explicit gap, the file was quarantined whole, and the terminal
    /// latched `degraded`.
    SegmentLost {
        stream: LogStream,
        range_start: u64,
        range_end: u64,
        reason: GapReason,
        file_name: String,
    },
    /// An active pointer referenced a row that is gone or not of its
    /// stream; the pointer was cleared so the next append creates a fresh
    /// segment (the watermark is untouched).
    ActivePointerCleared { stream: LogStream },
    /// A hole in one stream's ordered chain — a range the watermark
    /// consumed that no segment backs and no explicit gap covered — was
    /// recorded as an explicit gap and the terminal latched `degraded`.
    UncoveredHoleGapped {
        stream: LogStream,
        range_start: u64,
        range_end: u64,
    },
    /// A segment row whose indexed range extended past the stream's
    /// watermark-derived expected end was lost as corrupt (it claimed
    /// positions the watermark never consumed); its file was quarantined
    /// whole and the terminal latched `degraded`.
    BeyondWatermarkLost { stream: LogStream, segment_id: i64 },
    /// A retained floor sitting beyond the stream's watermark-derived
    /// expected end was clamped back under it (bookkeeping repair only:
    /// no data is lost, quarantined, or latched).
    RetainedFloorClamped { stream: LogStream },
}

pub(crate) async fn recover(
    store: &Store,
    root: &Path,
    log: &LogIdentity,
) -> Result<RecoveryReport, StorageError> {
    // Identity validation happens before any file or database mutation;
    // the same guards as a writer attach, so recovery can never adopt a
    // tampered or foreign identity either.
    let ident = ResolvedIdentity::parse(log)?;
    // The pass holds the log's exclusive writer lease for its whole
    // duration: it must never repair state underneath a live writer (and
    // a live writer must never append underneath a repairing pass). The
    // lease drops — releasing the lock — when the pass returns or fails.
    let _lease = WriterLease::acquire(root, &ident.key).await?;
    let (term, _created) = store.ensure_terminal(&ident.key, ident.header).await?;
    if term.terminal_uuid != ident.header.terminal_uuid {
        return Err(StorageError::TerminalUuidMismatch {
            stored: uuid::Uuid::from_bytes(term.terminal_uuid).to_string(),
            requested: ident.terminal_id,
        });
    }
    if term.log_epoch != ident.header.epoch {
        return Err(StorageError::EpochMismatch {
            stored: uuid::Uuid::from_bytes(term.log_epoch).to_string(),
            requested: ident.epoch,
        });
    }

    let mut actions = Vec::new();
    let rows = store.all_segments(&ident.key).await?;

    orphan_pass(root, &ident, &rows, &mut actions).await?;
    for row in &rows {
        row_pass(store, root, &ident, row, &mut actions).await?;
    }
    pointer_pass(store, &ident, &mut actions).await?;
    chain_pass(store, root, &ident, &mut actions).await?;
    let report = summarize(store, &ident, &actions).await?;
    Ok(report)
}

/// Build the domain-level report of a finished pass: whether anything
/// was repaired, the post-pass `degraded` latch, and the coalesced gap
/// state of both streams.
async fn summarize(
    store: &Store,
    ident: &ResolvedIdentity,
    actions: &[RecoveryAction],
) -> Result<RecoveryReport, StorageError> {
    let term = store
        .terminal(&ident.key)
        .await?
        .expect("terminal row ensured");
    let mut gaps = Vec::new();
    for stream in [LogStream::Normalized, LogStream::Raw] {
        for span in store.gaps(&ident.key, stream).await? {
            gaps.push(RecoveryGap {
                stream,
                start: span.start,
                end: span.end,
                reason: span.reason,
            });
        }
    }
    Ok(RecoveryReport {
        repaired: !actions.is_empty(),
        degraded: term.degraded,
        gaps,
        #[cfg(any(test, feature = "test-hooks"))]
        actions: actions.to_vec(),
    })
}

/// Quarantine whole segment files that no row claims. A file whose header
/// parses to this terminal's UUID belongs to this terminal in *some*
/// epoch (a crash window, or the old epoch after a destructive database
/// rebuild); a file whose header does not parse at all is unattributable
/// garbage on a storage-owned name. Both are quarantined whole and never
/// adopted. A file with a valid header of a different terminal is left
/// for that terminal's own recovery.
async fn orphan_pass(
    root: &Path,
    ident: &ResolvedIdentity,
    rows: &[SegmentRow],
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), StorageError> {
    let claimed: HashSet<&str> = rows.iter().map(|row| row.file_name.as_str()).collect();
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(root).await.map_err(io_error(root))?;
    while let Some(entry) = entries.next_entry().await.map_err(io_error(root))? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("seg-") && name.ends_with(".log")) && !claimed.contains(name.as_str())
        {
            names.push(name);
        }
    }
    // Directory order is arbitrary; sort so the pass (and its report) is
    // deterministic.
    names.sort();
    for name in names {
        let path = paths::segment_path(root, &name);
        let data = tokio::fs::read(&path).await.map_err(io_error(&path))?;
        let header = parse_header(&data);
        let ours = header
            .as_ref()
            .is_some_and(|h| h.terminal == ident.header.terminal_uuid);
        if ours || header.is_none() {
            quarantine_whole_file(root, &name).await?;
            actions.push(RecoveryAction::OrphanSegmentQuarantined { file_name: name });
        }
    }
    Ok(())
}

/// Classify and repair one segment row.
async fn row_pass(
    store: &Store,
    root: &Path,
    ident: &ResolvedIdentity,
    row: &SegmentRow,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), StorageError> {
    let path = paths::segment_path(root, &row.file_name);
    let data = match tokio::fs::read(&path).await {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The row's file is gone. Zero committed bytes means a crashed
            // creation: tombstone the whole row, adopt nothing.
            if row.committed_bytes == 0 || row.stream_range().is_none() {
                store
                    .tombstone_segment(&ident.key, row.segment_id, row.kind)
                    .await?;
                actions.push(RecoveryAction::ZeroByteRowTombstoned {
                    segment_id: row.segment_id,
                });
                return Ok(());
            }
            lose_segment(store, root, ident, row, GapReason::Missing, actions).await?;
            return Ok(());
        }
        Err(source) => return Err(StorageError::Io { path, source }),
    };

    let header_ok = header_matches(ident, row, &data);
    if row.committed_bytes == 0 {
        // Zero-byte pending tombstone rule: a row that committed nothing
        // is a crashed creation. The row is tombstoned whole and any file
        // under its name is quarantined whole (rename + directory fsync)
        // so the pending bytes survive as an artifact — header-valid or
        // not, its contents are never adopted. A crash between the
        // tombstone and the rename leaves the file unclaimed, which the
        // rerun's orphan pass quarantines.
        store
            .tombstone_segment(&ident.key, row.segment_id, row.kind)
            .await?;
        quarantine_whole_file(root, &row.file_name).await?;
        actions.push(RecoveryAction::ZeroByteRowTombstoned {
            segment_id: row.segment_id,
        });
        return Ok(());
    }

    // A committed range exists: the file must hold at least the committed
    // boundary with a valid header and a clean committed prefix whose
    // coordinates agree with the row's indexed range. Anything else is an
    // irrecoverable indexed segment.
    let boundary = row.committed_bytes;
    let truncated = (data.len() as u64) < boundary || boundary < SEGMENT_HEADER_LEN as u64;
    let clean = if truncated || !header_ok {
        false
    } else {
        let scan = scan_frames(
            &data[..boundary as usize],
            SEGMENT_HEADER_LEN,
            row.kind.segment_kind(),
        );
        scan.outcome == ScanOutcome::Clean && frames_match_row(&scan, row)
    };
    if !clean {
        let reason = if truncated && header_ok {
            GapReason::Truncated
        } else {
            GapReason::Corrupt
        };
        lose_segment(store, root, ident, row, reason, actions).await?;
        return Ok(());
    }

    // The committed prefix validates. Anything beyond it (a bad, partial,
    // or complete-but-unindexed tail) is quarantined as an artifact before
    // the live file is truncated back to the committed boundary.
    if data.len() as u64 > boundary {
        quarantine_tail(
            store,
            root,
            &row.file_name,
            &data[boundary as usize..],
            boundary,
        )
        .await?;
        actions.push(RecoveryAction::TailQuarantined {
            file_name: row.file_name.clone(),
            quarantined_bytes: data.len() as u64 - boundary,
            truncated_to: boundary,
        });
    }
    Ok(())
}

/// Record the loss of one irrecoverable indexed segment — the coalesced
/// gap for its whole range, the active-pointer clear, the row tombstone,
/// and the `degraded` latch, **one transaction, one commit** — and only
/// then quarantine its file whole. A crash after the commit leaves a
/// fully recorded loss plus an unclaimed file (the rerun's orphan pass
/// quarantines it); a crash before it leaves the row intact and the
/// rerun records the loss again. Never a half-recorded loss.
async fn lose_segment(
    store: &Store,
    root: &Path,
    ident: &ResolvedIdentity,
    row: &SegmentRow,
    reason: GapReason,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), StorageError> {
    if let Some((start, end)) = row.stream_range() {
        store
            .record_loss(
                &ident.key,
                row.kind,
                GapSpan { start, end, reason },
                row.segment_id,
            )
            .await?;
        // The failpoint sits *after* the commit on purpose: dying here is
        // the proof that the loss record is atomic (gap + pointer clear +
        // tombstone + latch all durable, or none of them).
        store.hit(crate::crash::CrashPoint::RecoveryRowsCommitted);
        actions.push(RecoveryAction::SegmentLost {
            stream: row.kind,
            range_start: start,
            range_end: end,
            reason,
            file_name: row.file_name.clone(),
        });
    } else {
        // An irrecoverable row whose indexed range is empty lost nothing:
        // tombstone it whole (clearing any pointer) without a gap.
        store
            .tombstone_segment(&ident.key, row.segment_id, row.kind)
            .await?;
        store.hit(crate::crash::CrashPoint::RecoveryRowsCommitted);
        actions.push(RecoveryAction::ZeroByteRowTombstoned {
            segment_id: row.segment_id,
        });
    }
    quarantine_whole_file(root, &row.file_name).await?;
    Ok(())
}

/// Clear active pointers that reference a missing row or a row of the
/// other stream. The watermark is never touched: numbering continues at
/// `watermark + 1` in a fresh segment.
async fn pointer_pass(
    store: &Store,
    ident: &ResolvedIdentity,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), StorageError> {
    let term = store
        .terminal(&ident.key)
        .await?
        .expect("terminal row ensured");
    for (stream, pointer) in [
        (LogStream::Normalized, term.active_normalized_segment),
        (LogStream::Raw, term.active_raw_segment),
    ] {
        let Some(segment_id) = pointer else { continue };
        // A pointer to a sealed row of its own stream is legitimate (a
        // mid-rotation crash leaves it behind; attach simply creates a
        // fresh segment on the next append). Only a dangling or
        // cross-stream pointer is broken bookkeeping.
        let ok = match store.segment(segment_id).await {
            Ok(row) => row.kind == stream,
            Err(StorageError::Database(_)) => false,
            Err(error) => return Err(error),
        };
        if !ok {
            // Clear the stale pointer only: the referenced row (if any)
            // keeps its data. A sealed row behind a stale pointer is normal
            // after a mid-rotation crash, never a loss.
            store
                .clear_active_pointer(&ident.key, stream, segment_id)
                .await?;
            actions.push(RecoveryAction::ActivePointerCleared { stream });
        }
    }
    Ok(())
}

/// One live range of one stream's ordered chain of segment rows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChainLink {
    pub segment_id: i64,
    /// Exclusive range start in the stream's coordinates.
    pub start: u64,
    /// Exclusive range end in the stream's coordinates.
    pub end: u64,
}

/// How one stream's ordered chain of segment rows violates continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainViolation {
    /// A row's range starts behind the chain cursor (an overlap with the
    /// preceding row, or a start below the retained floor): fabricated
    /// continuity. The row must be lost as corrupt.
    Overlap {
        segment_id: i64,
        start: u64,
        end: u64,
    },
    /// A hole in the chain — head, mid, or tail — that no explicit gap
    /// record covers: an unrecorded loss.
    Hole { start: u64, end: u64 },
    /// A row's range extends past the stream's expected end derived from
    /// the watermark (normalized: `watermark + 1`; raw: `watermark`): it
    /// claims positions the watermark never consumed, so the chain is
    /// validated *against* the watermark, not merely up to its first
    /// hole. The row must be lost as corrupt — never adopted as
    /// continuity beyond the durable watermark.
    BeyondWatermark {
        segment_id: i64,
        start: u64,
        end: u64,
    },
    /// The retained floor sits beyond the watermark-derived expected end:
    /// broken bookkeeping (a floor may only advance under positions the
    /// watermark consumed), repaired by clamping it back under the end.
    FloorBeyondWatermark { floor: u64, expected_end: u64 },
}

impl ChainViolation {
    /// Human-readable, domain-level description (no storage internals).
    /// Consumed by the committed-prefix read surface, which is
    /// `test-hooks`-only in S2 (the query read API arrives in S4).
    #[allow(dead_code)]
    pub(crate) fn detail(&self, stream: LogStream) -> String {
        let kind = stream.db_text();
        match self {
            ChainViolation::Overlap {
                segment_id,
                start,
                end,
            } => format!(
                "{kind} segment ranges overlap at [{start},{end}) (row {segment_id}); \
                 run recovery before reading"
            ),
            ChainViolation::Hole { start, end } => format!(
                "{kind} chain hole [{start},{end}) is not covered by an explicit gap; \
                 run recovery before reading"
            ),
            ChainViolation::BeyondWatermark {
                segment_id,
                start,
                end,
            } => format!(
                "{kind} segment range [{start},{end}) (row {segment_id}) extends past the \
                 watermark; run recovery before reading"
            ),
            ChainViolation::FloorBeyondWatermark {
                floor,
                expected_end,
            } => format!(
                "{kind} retained floor {floor} sits beyond the watermark's end \
                 {expected_end}; run recovery before reading"
            ),
        }
    }
}

/// Whether one explicit gap record covers the hole `[start, end)`.
fn covered_by(gaps: &[GapSpan], start: u64, end: u64) -> bool {
    gaps.iter().any(|gap| gap.start <= start && end <= gap.end)
}

/// Validate one stream's ordered cross-segment chain against its
/// retained floor, its watermark, and its explicit gap records. `links`
/// must hold the non-empty ranges of the stream's live rows, ordered by
/// `(start, segment_id)`. The chain must run strictly forward from the
/// retained floor to the stream's expected end (normalized: `watermark +
/// 1`, the exclusive end of the last accepted line; raw: `watermark`,
/// the exclusive end offset) — never beyond it: the retained floor and
/// every link's end must stay at or under the expected end — and every
/// hole must be covered by a gap — a coarsened gap may legitimately be
/// wider than the hole it covers, including past the expected end.
pub(crate) fn validate_chain(
    stream: LogStream,
    retained_floor: u64,
    watermark: u64,
    links: &[ChainLink],
    gaps: &[GapSpan],
) -> Result<(), ChainViolation> {
    let expected_end = match stream {
        LogStream::Normalized => watermark.saturating_add(1),
        LogStream::Raw => watermark,
    };
    if retained_floor > expected_end {
        return Err(ChainViolation::FloorBeyondWatermark {
            floor: retained_floor,
            expected_end,
        });
    }
    let mut cursor = retained_floor;
    for link in links {
        if link.start < cursor {
            return Err(ChainViolation::Overlap {
                segment_id: link.segment_id,
                start: link.start,
                end: link.end,
            });
        }
        if link.end > expected_end {
            return Err(ChainViolation::BeyondWatermark {
                segment_id: link.segment_id,
                start: link.start,
                end: link.end,
            });
        }
        if link.start > cursor && !covered_by(gaps, cursor, link.start) {
            return Err(ChainViolation::Hole {
                start: cursor,
                end: link.start,
            });
        }
        cursor = link.end;
    }
    if cursor < expected_end && !covered_by(gaps, cursor, expected_end) {
        return Err(ChainViolation::Hole {
            start: cursor,
            end: expected_end,
        });
    }
    Ok(())
}

/// Validate and repair the ordered cross-segment chain of both streams.
/// Each repair is idempotent and strictly shrinks the remaining work (an
/// overlap repair removes a row; a hole repair covers a hole that stays
/// covered, because coalescing only widens), so the loop converges and a
/// rerun of a completed pass takes no action.
async fn chain_pass(
    store: &Store,
    root: &Path,
    ident: &ResolvedIdentity,
    actions: &mut Vec<RecoveryAction>,
) -> Result<(), StorageError> {
    loop {
        let term = store
            .terminal(&ident.key)
            .await?
            .expect("terminal row ensured");
        let rows = store.all_segments(&ident.key).await?;
        let mut repaired = false;
        for stream in [LogStream::Normalized, LogStream::Raw] {
            let mut links: Vec<ChainLink> = rows
                .iter()
                .filter(|row| {
                    row.kind == stream
                        && (row.state == "active" || row.state == "sealed")
                        && row.stream_range().is_some()
                })
                .map(|row| {
                    let (start, end) = row.stream_range().expect("non-empty range");
                    ChainLink {
                        segment_id: row.segment_id,
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
            let gaps = store.gaps(&ident.key, stream).await?;
            match validate_chain(stream, floor, watermark, &links, &gaps) {
                Ok(()) => continue,
                Err(violation @ ChainViolation::Overlap { .. })
                | Err(violation @ ChainViolation::BeyondWatermark { .. }) => {
                    // Fabricated continuity, or a range the watermark never
                    // consumed: either way the row is corrupt and lost whole
                    // (explicit gap + tombstone + latch in one transaction,
                    // file quarantined), so no continuity is ever fabricated
                    // beyond the durable watermark.
                    let (segment_id, beyond) = match violation {
                        ChainViolation::Overlap { segment_id, .. } => (segment_id, false),
                        ChainViolation::BeyondWatermark { segment_id, .. } => (segment_id, true),
                        other => unreachable!("matched violation escaped: {other:?}"),
                    };
                    let row = rows
                        .iter()
                        .find(|row| row.segment_id == segment_id)
                        .expect("violating row just read");
                    lose_segment(store, root, ident, row, GapReason::Corrupt, actions).await?;
                    if beyond {
                        actions.push(RecoveryAction::BeyondWatermarkLost { stream, segment_id });
                    }
                    repaired = true;
                    break;
                }
                Err(ChainViolation::Hole { start, end }) => {
                    store
                        .record_hole(
                            &ident.key,
                            stream,
                            GapSpan {
                                start,
                                end,
                                reason: GapReason::Missing,
                            },
                        )
                        .await?;
                    actions.push(RecoveryAction::UncoveredHoleGapped {
                        stream,
                        range_start: start,
                        range_end: end,
                    });
                    repaired = true;
                    break;
                }
                Err(ChainViolation::FloorBeyondWatermark { expected_end, .. }) => {
                    // Bookkeeping repair only: clamp the floor back under
                    // the watermark-derived expected end. Never decreases
                    // a watermark and never fabricates or loses data; with
                    // any live link present the row-level violations above
                    // fire first, so this clamp can never strand a valid
                    // row below the new floor.
                    store
                        .clamp_retained_floor(&ident.key, stream, expected_end)
                        .await?;
                    actions.push(RecoveryAction::RetainedFloorClamped { stream });
                    repaired = true;
                    break;
                }
            }
        }
        if !repaired {
            return Ok(());
        }
    }
}

fn parse_header(data: &[u8]) -> Option<SegmentHeader> {
    if data.len() < SEGMENT_HEADER_LEN {
        return None;
    }
    SegmentHeader::parse(data).ok()
}

/// Whether the file's header is a valid header for exactly this row:
/// kind, terminal UUID, epoch, and segment id must all match.
fn header_matches(ident: &ResolvedIdentity, row: &SegmentRow, data: &[u8]) -> bool {
    parse_header(data).is_some_and(|header| {
        header.kind == row.kind.segment_kind()
            && header.terminal == ident.header.terminal_uuid
            && header.epoch == ident.header.epoch
            && header.segment_id == row.segment_id as u64
    })
}

/// Whether a clean scan of one segment's committed prefix agrees with the
/// row's indexed range: a normalized segment must open on its `first_line`
/// and end one past its exclusive `last_line` on the last frame's line —
/// and its final frame must carry [`FRAME_FLAG_LINE_END`, because every
/// committed normalized batch ends on a line boundary, so a "complete"
/// prefix whose last frame is a mid-line chunk (a checksummed tamper that
/// cleared the flag, or a writer that lost the tail chunk) claims a
/// committed range over an unfinished line and is refused; a raw segment
/// must open on its `first_offset` and end exactly at `last_offset` (raw
/// frames carry no line flags, so that stream's rule is unchanged). A
/// checksummed file that disagrees with its index can therefore never
/// fabricate continuity (recovery loses it as corrupt; reads refuse until
/// recovery ran). An empty prefix only matches a row whose indexed range
/// is empty.
pub(crate) fn frames_match_row(scan: &ScanReport, row: &SegmentRow) -> bool {
    let Some(first) = scan.frames.first() else {
        return row.stream_range().is_none();
    };
    let last = scan.frames[scan.frames.len() - 1].header.clone();
    let first = first.header.clone();
    match row.kind {
        LogStream::Normalized => match row.first_line {
            Some(first_line) => {
                first.line == first_line
                    && last.line.checked_add(1) == row.last_line
                    && last.flags & FRAME_FLAG_LINE_END != 0
            }
            None => false,
        },
        LogStream::Raw => {
            first.line_offset == row.first_offset
                && last.line_offset.checked_add(u64::from(last.payload_len))
                    == Some(row.last_offset)
        }
    }
}

/// Quarantine a whole segment file by rename, then fsync the directory so
/// the rename is durable. Source and destination directory are the same
/// storage root, so one directory sync persists both sides; its failure
/// (like every step's) propagates.
async fn quarantine_whole_file(root: &Path, file_name: &str) -> Result<(), StorageError> {
    let from = paths::segment_path(root, file_name);
    let to = root.join(paths::quarantine_file_name(file_name));
    match tokio::fs::rename(&from, &to).await {
        Ok(()) => {}
        // The file is already gone (a missing-segment loss, or a rerun
        // after an earlier pass quarantined it): nothing to rename.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(StorageError::Io { path: from, source }),
    }
    paths::fsync_dir(root).await?;
    Ok(())
}

/// Persist the uncommitted tail as a quarantine artifact — write it,
/// `sync_data` it, fsync the directory so the artifact's entry is
/// durable — and only then truncate the live file back to `boundary` and
/// sync it. A crash in between leaves a durable artifact plus an
/// untruncated live file, which the rerun converges (the artifact is
/// rewritten deterministically onto the same name).
async fn quarantine_tail(
    store: &Store,
    root: &Path,
    file_name: &str,
    tail: &[u8],
    boundary: u64,
) -> Result<(), StorageError> {
    let artifact_path = root.join(paths::quarantine_file_name(file_name));
    let mut artifact = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&artifact_path)
        .await
        .map_err(io_error(&artifact_path))?;
    artifact
        .write_all(tail)
        .await
        .map_err(io_error(&artifact_path))?;
    artifact
        .sync_data()
        .await
        .map_err(io_error(&artifact_path))?;
    drop(artifact);
    paths::fsync_dir(root).await?;
    store.hit(crate::crash::CrashPoint::RecoveryArtifactSynced);
    let live_path = paths::segment_path(root, file_name);
    let live = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&live_path)
        .await
        .map_err(io_error(&live_path))?;
    live.set_len(boundary).await.map_err(io_error(&live_path))?;
    live.sync_data().await.map_err(io_error(&live_path))?;
    Ok(())
}

/// The durable state of one terminal log as an comparable value: the
/// terminal row, every segment row, every gap record, and every segment
/// or quarantine file with its length and content checksum. Two
/// consecutive recoveries of an already-recovered log observe equal
/// snapshots (test builds only; the query read API belongs to S4).
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub terminal_uuid: [u8; 16],
    pub log_epoch: [u8; 16],
    pub line_watermark: u64,
    pub raw_watermark: u64,
    pub retained_first_line: u64,
    pub retained_first_offset: u64,
    pub active_normalized_segment: Option<i64>,
    pub active_raw_segment: Option<i64>,
    pub degraded: bool,
    pub refuse_new_start: bool,
    pub segments: Vec<SnapshotSegment>,
    pub gaps: Vec<SnapshotGap>,
    pub files: Vec<SnapshotFile>,
}

#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSegment {
    pub segment_id: i64,
    pub kind: LogStream,
    pub state: String,
    pub committed_bytes: u64,
    pub fsynced_bytes: u64,
    pub first_line: Option<u64>,
    pub last_line: Option<u64>,
    pub first_offset: u64,
    pub last_offset: u64,
}

#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotGap {
    pub stream: LogStream,
    pub start: u64,
    pub end: u64,
    pub reason: GapReason,
}

#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFile {
    pub name: String,
    pub len: u64,
    pub crc32: u32,
}

#[cfg(any(test, feature = "test-hooks"))]
pub(crate) async fn snapshot(
    store: &Store,
    root: &Path,
    log: &LogIdentity,
) -> Result<RecoverySnapshot, StorageError> {
    let ident = ResolvedIdentity::parse(log)?;
    let term = store
        .terminal(&ident.key)
        .await?
        .ok_or_else(|| StorageError::UnknownLog(ident.terminal_id.clone()))?;
    let segments = store
        .all_segments(&ident.key)
        .await?
        .into_iter()
        .map(|row| SnapshotSegment {
            segment_id: row.segment_id,
            kind: row.kind,
            state: row.state,
            committed_bytes: row.committed_bytes,
            fsynced_bytes: row.fsynced_bytes,
            first_line: row.first_line,
            last_line: row.last_line,
            first_offset: row.first_offset,
            last_offset: row.last_offset,
        })
        .collect();
    let mut gaps = Vec::new();
    for stream in [LogStream::Normalized, LogStream::Raw] {
        for span in store.gaps(&ident.key, stream).await? {
            gaps.push(SnapshotGap {
                stream,
                start: span.start,
                end: span.end,
                reason: span.reason,
            });
        }
    }
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(root).await.map_err(io_error(root))?;
    while let Some(entry) = entries.next_entry().await.map_err(io_error(root))? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_log = name.starts_with("seg-") && name.ends_with(".log");
        let is_quarantine = name.starts_with("quarantine-");
        if !is_log && !is_quarantine {
            continue;
        }
        let path = entry.path();
        let data = tokio::fs::read(&path).await.map_err(io_error(&path))?;
        files.push(SnapshotFile {
            name,
            len: data.len() as u64,
            crc32: crate::frame::crc32(&data),
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(RecoverySnapshot {
        terminal_uuid: term.terminal_uuid,
        log_epoch: term.log_epoch,
        line_watermark: term.line_watermark,
        raw_watermark: term.raw_watermark,
        retained_first_line: term.retained_first_line,
        retained_first_offset: term.retained_first_offset,
        active_normalized_segment: term.active_normalized_segment,
        active_raw_segment: term.active_raw_segment,
        degraded: term.degraded,
        refuse_new_start: term.refuse_new_start,
        segments,
        gaps,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SegmentRow;
    use crate::frame::{
        FRAME_FLAG_LINE_END, FRAME_HEADER_LEN, FRAME_KIND_LINE, FRAME_KIND_RAW, FrameHeader,
        SEGMENT_KIND_NORMALIZED, SEGMENT_KIND_RAW, encode_frame,
    };

    fn line_frame(seq: u64, line: u64, offset: u64, payload: &[u8], flags: u8) -> Vec<u8> {
        encode_frame(
            &FrameHeader {
                kind: FRAME_KIND_LINE,
                flags,
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: payload.len() as u32,
            },
            payload,
        )
        .unwrap()
    }

    fn normalized_row(first_line: u64, last_line: u64) -> SegmentRow {
        SegmentRow {
            segment_id: 1,
            kind: LogStream::Normalized,
            file_name: "seg-000001.log".into(),
            committed_bytes: 0,
            fsynced_bytes: 0,
            state: "active".into(),
            first_line: Some(first_line),
            last_line: Some(last_line),
            first_offset: 0,
            last_offset: 0,
        }
    }

    /// A complete normalized committed prefix whose final frame carries
    /// the line-end flag matches its indexed range.
    #[test]
    fn normalized_prefix_matches_only_when_the_final_frame_ends_its_line() {
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&line_frame(0, 1, 0, b"one", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&line_frame(1, 2, 0, b"two", FRAME_FLAG_LINE_END));
        let scan = scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED);
        assert_eq!(scan.outcome, ScanOutcome::Clean);
        assert!(frames_match_row(&scan, &normalized_row(1, 3)));

        // Same bytes, final flag cleared and the header checksum fixed
        // (a checksummed tamper): the scan stays clean and the line
        // numbers still match, but the row claims a committed range over
        // an unfinished line — the match must refuse.
        let last = buf.len() - (FRAME_HEADER_LEN + 3 + 4);
        buf[last + 7] = 0; // clear FRAME_FLAG_LINE_END
        let fixed_crc = crate::frame::crc32(&buf[last..last + 36]);
        buf[last + 36..last + 40].copy_from_slice(&fixed_crc.to_le_bytes());
        let scan = scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED);
        assert_eq!(
            scan.outcome,
            ScanOutcome::Clean,
            "the tamper is checksummed"
        );
        assert!(
            !frames_match_row(&scan, &normalized_row(1, 3)),
            "a mid-line final frame must not match the committed range"
        );
    }

    /// A multi-chunk line: the final frame is the one that must carry the
    /// flag; an earlier chunk carrying it is refused by the scanner, and
    /// the prefix only matches when the *last* frame ends the line.
    #[test]
    fn multi_chunk_line_prefix_requires_the_last_chunk_to_end_the_line() {
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&line_frame(0, 1, 0, b"ab", 0));
        buf.extend_from_slice(&line_frame(1, 1, 2, b"cde", FRAME_FLAG_LINE_END));
        let scan = scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED);
        assert_eq!(scan.outcome, ScanOutcome::Clean);
        assert!(frames_match_row(&scan, &normalized_row(1, 2)));

        // Clear the flag on the final (mid-line) chunk with a fixed
        // checksum: same refusal as the single-chunk case.
        let last = buf.len() - (FRAME_HEADER_LEN + 3 + 4);
        buf[last + 7] = 0;
        let fixed_crc = crate::frame::crc32(&buf[last..last + 36]);
        buf[last + 36..last + 40].copy_from_slice(&fixed_crc.to_le_bytes());
        let scan = scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED);
        assert_eq!(scan.outcome, ScanOutcome::Clean);
        assert!(!frames_match_row(&scan, &normalized_row(1, 2)));
    }

    /// The raw stream's rule is unchanged: raw frames carry no line flags,
    /// so its match never consults (or rejects on) the line-end bit.
    #[test]
    fn raw_prefix_match_is_unchanged_by_the_line_end_rule() {
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        let mut stream_offset = 0u64;
        for (seq, chunk) in [(0u64, b"one".as_slice()), (1u64, b"two".as_slice())] {
            buf.extend_from_slice(
                &encode_frame(
                    &FrameHeader {
                        kind: FRAME_KIND_RAW,
                        flags: 0,
                        frame_seq: seq,
                        line: 0,
                        line_offset: stream_offset,
                        payload_len: chunk.len() as u32,
                    },
                    chunk,
                )
                .unwrap(),
            );
            stream_offset += chunk.len() as u64;
        }
        let row = SegmentRow {
            kind: LogStream::Raw,
            first_offset: 0,
            last_offset: 6,
            ..normalized_row(0, 0)
        };
        let scan = scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_RAW);
        assert_eq!(scan.outcome, ScanOutcome::Clean);
        assert!(frames_match_row(&scan, &row));
    }
}
