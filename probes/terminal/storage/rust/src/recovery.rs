//! Throwaway recovery scanner/repair for probe C. NOT production code.
//!
//! Recovery scans only bounded lengths, checksums, sequence numbers and line
//! offsets; it never adopts uncommitted or unverifiable data. Outcomes are
//! classified clean / partial / corrupt / orphan / missing / truncated /
//! epoch-mismatch, every repair is recorded, and running recovery twice on
//! the same state must produce the same result (idempotent).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crash::CrashCtl;
use crate::db::{GapRow, SegmentRow, Store};
use crate::frame::{SEGMENT_HEADER_LEN, ScanOutcome, SegmentHeader, scan_frames};

pub const SEGMENT_FILE_PREFIX: &str = "seg-";
pub const SEGMENT_FILE_SUFFIX: &str = ".log";
pub const QUARANTINE_DIR: &str = "quarantine";
/// Recovery never scans unbounded data: segment files are writer-rotated at
/// 256 KiB, so anything larger than this bound is foreign and classified as
/// corruption without reading it.
pub const MAX_SCAN_FILE: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentClass {
    Clean,
    Partial,
    Corrupt,
    Orphan,
    Missing,
    Truncated,
    EpochMismatch,
    /// Row exists but `committed_bytes == 0`: the writer died between
    /// segment creation and its first line commit, so nothing in that file
    /// was ever published. The file is quarantined whole, never adopted,
    /// and no gap is fabricated (the available range was empty).
    Pending,
}

impl SegmentClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            SegmentClass::Clean => "clean",
            SegmentClass::Partial => "partial",
            SegmentClass::Corrupt => "corrupt",
            SegmentClass::Orphan => "orphan",
            SegmentClass::Missing => "missing",
            SegmentClass::Truncated => "truncated",
            SegmentClass::EpochMismatch => "epoch-mismatch",
            SegmentClass::Pending => "pending",
        }
    }
}

impl std::fmt::Display for SegmentClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentOutcome {
    pub segment_id: Option<i64>,
    pub file_name: String,
    pub class: SegmentClass,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// True when the terminal row did not exist and was created with a fresh
    /// epoch (first run, or the database was deleted/replaced).
    pub created_terminal: bool,
    pub epoch: [u8; 16],
    /// True when a destructive rebuild rotated the epoch this run.
    pub epoch_rotated: bool,
    pub destructive_rebuild: bool,
    pub outcomes: Vec<SegmentOutcome>,
    pub actions: Vec<String>,
    pub gaps: Vec<GapRow>,
    pub degraded: bool,
    pub line_watermark: u64,
}

fn is_segment_file_name(name: &str) -> bool {
    name.starts_with(SEGMENT_FILE_PREFIX) && name.ends_with(SEGMENT_FILE_SUFFIX)
}

fn quarantine_dir(root: &Path) -> PathBuf {
    root.join(QUARANTINE_DIR)
}

fn unique_in(dir: &Path, name: &str) -> PathBuf {
    let mut candidate = dir.join(name);
    let mut n = 1u32;
    while candidate.exists() {
        candidate = dir.join(format!("{name}.{n}"));
        n += 1;
    }
    candidate
}

/// Create the quarantine directory when missing and make its directory
/// entry durable in the storage root before anything is written into it.
fn ensure_quarantine_dir(root: &Path) -> Result<()> {
    let qdir = quarantine_dir(root);
    if !qdir.exists() {
        std::fs::create_dir_all(&qdir).context("create quarantine dir")?;
        crate::fsync_dir(root).with_context(|| {
            format!(
                "fsync root {} after quarantine dir creation",
                root.display()
            )
        })?;
    }
    Ok(())
}

/// Move a segment file into the quarantine directory (never adopted again).
/// Both directory mutations are durably synchronized and every sync error
/// propagates: the quarantine entry is fsynced so the artifact cannot be
/// lost to power loss, and the storage root is fsynced so the source name
/// cannot resurrect after a crash.
fn quarantine_file(root: &Path, file_name: &str, ctl: &CrashCtl) -> Result<String> {
    ensure_quarantine_dir(root)?;
    let qdir = quarantine_dir(root);
    let src = root.join(file_name);
    let dst = unique_in(&qdir, file_name);
    std::fs::rename(&src, &dst).with_context(|| format!("quarantine {}", src.display()))?;
    crate::fsync_dir(&qdir)
        .with_context(|| format!("fsync quarantine dir {} after rename", qdir.display()))?;
    crate::fsync_dir(root)
        .with_context(|| format!("fsync root {} after quarantine rename", root.display()))?;
    ctl.hit("rec_file_quarantined");
    Ok(dst.file_name().unwrap().to_string_lossy().into_owned())
}

/// Save a copy of removed tail bytes as a quarantine artifact, sync the
/// artifact and its directory, and only then truncate the live file to
/// `keep` bytes (also synced). Ordering matters: the removed bytes are
/// durable before the live file loses them.
fn quarantine_tail_bytes(
    root: &Path,
    file_name: &str,
    keep: u64,
    bytes: &[u8],
    ctl: &CrashCtl,
) -> Result<String> {
    ensure_quarantine_dir(root)?;
    let qdir = quarantine_dir(root);
    let dst = unique_in(&qdir, &format!("{file_name}.tail"));
    let mut file = std::fs::File::create(&dst)
        .with_context(|| format!("create artifact {}", dst.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write artifact {}", dst.display()))?;
    file.sync_data()
        .with_context(|| format!("sync artifact {}", dst.display()))?;
    drop(file);
    crate::fsync_dir(&qdir)
        .with_context(|| format!("fsync quarantine dir {} after artifact", qdir.display()))?;
    ctl.hit("rec_artifact_written");
    truncate_file(&root.join(file_name), keep)?;
    ctl.hit("rec_tail_truncated");
    Ok(dst.file_name().unwrap().to_string_lossy().into_owned())
}

fn truncate_file(path: &Path, keep: u64) -> Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(keep)
        .with_context(|| format!("truncate {} to {keep}", path.display()))?;
    file.sync_data()?;
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_SCAN_FILE {
        bail!(
            "{} exceeds recovery scan bound {}",
            path.display(),
            MAX_SCAN_FILE
        );
    }
    // Length already bounded; payload sizes are validated before any use.
    Ok(std::fs::read(path)?)
}

struct Examined {
    row: SegmentRow,
    class: SegmentClass,
    detail: String,
    /// End of the last valid frame inside the committed region (repair target).
    valid_end: u64,
    /// Line number of the last valid frame in the committed region.
    last_good_line: Option<u64>,
    /// Whole file must be quarantined (header-level failure).
    quarantine_whole: bool,
    epoch_mismatch: bool,
    /// Repair the row state (missing/quarantined) after truncation.
    repaired_state: Option<&'static str>,
}

/// Examine one DB-referenced segment file. Read-only; repairs happen later.
fn examine(row: &SegmentRow, term: &crate::db::TerminalRow, root: &Path) -> Result<Examined> {
    let mut ex = Examined {
        row: row.clone(),
        class: SegmentClass::Clean,
        detail: String::new(),
        valid_end: SEGMENT_HEADER_LEN as u64,
        last_good_line: None,
        quarantine_whole: false,
        epoch_mismatch: false,
        repaired_state: None,
    };
    let path = root.join(&row.file_name);
    // A row with zero committed bytes has never published anything: the
    // writer died between segment creation and its first line commit, with
    // or without the (never-committed) file on disk. Classify it pending —
    // before any file-based classification.
    if row.committed_bytes == 0 {
        ex.class = SegmentClass::Pending;
        ex.detail = "no committed bytes: writer died before the first line commit".into();
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }
    if !path.exists() {
        ex.class = SegmentClass::Missing;
        ex.detail = "segment file referenced by DB is absent".into();
        ex.repaired_state = Some("missing");
        return Ok(ex);
    }
    let meta_len = std::fs::metadata(&path)?.len();
    if meta_len > MAX_SCAN_FILE {
        ex.class = SegmentClass::Corrupt;
        ex.detail = format!("file length {meta_len} above scan bound");
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }
    let data = read_bounded(&path)?;
    if data.len() < SEGMENT_HEADER_LEN {
        ex.class = SegmentClass::Truncated;
        ex.detail = "file shorter than the 64-byte segment header".into();
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }
    let header = match SegmentHeader::parse(&data[..SEGMENT_HEADER_LEN]) {
        Ok(h) => h,
        Err(e) => {
            ex.class = SegmentClass::Corrupt;
            ex.detail = format!("segment header: {e}");
            ex.quarantine_whole = true;
            ex.repaired_state = Some("quarantined");
            return Ok(ex);
        }
    };
    if header.terminal != term.terminal_uuid {
        ex.class = SegmentClass::Corrupt;
        ex.detail = "segment header terminal uuid does not match DB".into();
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }
    if header.epoch != term.log_epoch {
        ex.class = SegmentClass::EpochMismatch;
        ex.detail = "segment header epoch does not match terminal log_epoch".into();
        ex.epoch_mismatch = true;
        return Ok(ex);
    }
    if header.segment_id != u64::try_from(row.segment_id).unwrap_or(u64::MAX) {
        ex.class = SegmentClass::Corrupt;
        ex.detail = "segment header id does not match DB row".into();
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }

    let committed = row.committed_bytes;
    if committed < SEGMENT_HEADER_LEN as u64 {
        ex.class = SegmentClass::Corrupt;
        ex.detail = format!("committed_bytes {committed} below segment header length");
        ex.quarantine_whole = true;
        ex.repaired_state = Some("quarantined");
        return Ok(ex);
    }

    // Committed region scan. Everything before the first bad frame stays
    // readable; the available range shrinks to it.
    let committed_end = (committed as usize).min(data.len());
    let committed_scan = scan_frames(&data[..committed_end], SEGMENT_HEADER_LEN);
    let committed_frames_end = committed_scan
        .frames
        .last()
        .map(|f| f.end)
        .unwrap_or(SEGMENT_HEADER_LEN);
    ex.valid_end = committed_frames_end as u64;
    ex.last_good_line = committed_scan.frames.last().map(|f| f.header.line);

    let file_len = data.len() as u64;
    if file_len < committed {
        ex.class = SegmentClass::Truncated;
        ex.detail = format!("committed region lost: file {file_len} < committed {committed}");
        return Ok(ex);
    }
    match committed_scan.outcome {
        ScanOutcome::Clean => {}
        ScanOutcome::PartialTail => {
            // The committed boundary itself cuts a frame: index corruption.
            ex.class = SegmentClass::Corrupt;
            ex.detail = "committed boundary cuts a frame in half".into();
            return Ok(ex);
        }
        ScanOutcome::CorruptAt(off, why) => {
            ex.class = SegmentClass::Corrupt;
            ex.detail = format!("committed frame at byte {off}: {why}");
            return Ok(ex);
        }
    }

    // Tail beyond the committed boundary: never adopted, always truncated
    // away (with a quarantine artifact), whether complete or damaged.
    if file_len > committed {
        let tail_scan = scan_frames(&data, committed as usize);
        let scan_note = match tail_scan.outcome {
            ScanOutcome::Clean => "uncommitted complete frames beyond committed boundary",
            ScanOutcome::PartialTail => "partial (half) frame at tail",
            ScanOutcome::CorruptAt(_, why) => why,
        };
        ex.class = SegmentClass::Partial;
        ex.detail = format!("tail not committed: {scan_note}");
        ex.valid_end = committed;
    }
    Ok(ex)
}

/// Full recovery pass for one terminal in one storage root directory.
pub async fn recover(store: &Store, root: &Path, terminal_id: &str) -> Result<RecoveryReport> {
    let created_terminal = if !store.terminal_exists(terminal_id).await? {
        let uuid = Uuid::now_v7().into_bytes();
        let epoch = Uuid::now_v7().into_bytes();
        store.create_terminal(terminal_id, uuid, epoch).await?;
        true
    } else {
        false
    };
    let term = store.get_terminal(terminal_id).await?;

    let mut outcomes: Vec<SegmentOutcome> = Vec::new();
    let mut actions: Vec<String> = Vec::new();
    let mut any_epoch_mismatch = false;

    let rows = store.segments(terminal_id).await?;
    let mut examined: Vec<Examined> = Vec::new();
    for row in &rows {
        if row.state == "quarantined" {
            // Tombstone row: its file was already moved away in an earlier
            // recovery. Nothing to examine here.
            continue;
        }
        let ex = examine(row, &term, root)?;
        if ex.epoch_mismatch {
            any_epoch_mismatch = true;
        }
        outcomes.push(SegmentOutcome {
            segment_id: Some(row.segment_id),
            file_name: row.file_name.clone(),
            class: ex.class,
            detail: ex.detail.clone(),
        });
        examined.push(ex);
    }

    let mut destructive_rebuild = false;
    let mut epoch_rotated = false;
    let mut epoch = term.log_epoch;
    let mut degraded_set = false;

    if any_epoch_mismatch {
        // Destructive rebuild: quarantine every remaining segment file of
        // this terminal, rotate the epoch, keep the line watermark. Old
        // cursors then fail validation (see TerminalRow::validate_cursor).
        for row in &rows {
            if row.state != "quarantined" && root.join(&row.file_name).exists() {
                let moved = quarantine_file(root, &row.file_name, store.crash())?;
                actions.push(format!(
                    "epoch-mismatch: quarantined {} as quarantine/{moved}",
                    row.file_name
                ));
            }
        }
        let new_epoch = Uuid::now_v7().into_bytes();
        store.destructive_rebuild(terminal_id, new_epoch).await?;
        epoch = new_epoch;
        epoch_rotated = true;
        destructive_rebuild = true;
        actions.push(format!(
            "destructive rebuild: epoch rotated to {}; line watermark {} preserved; old cursors rejected",
            Uuid::from_bytes(new_epoch),
            term.line_watermark
        ));
        // Re-classify the mismatching segments in the report.
        for outcome in &mut outcomes {
            if outcome.class == SegmentClass::EpochMismatch {
                outcome.detail.push_str("; handled by destructive rebuild");
            }
        }
    } else {
        for ex in examined {
            let row = &ex.row;
            match ex.class {
                SegmentClass::Clean => {
                    if row.state == "missing" {
                        // The file reappeared and verifies against the index.
                        store
                            .repair_segment(
                                row.segment_id,
                                row.committed_bytes,
                                row.fsynced_bytes,
                                row.last_line,
                                "sealed",
                            )
                            .await?;
                        actions.push(format!(
                            "reappeared segment {} verified; state=sealed",
                            row.segment_id
                        ));
                    }
                }
                SegmentClass::Missing => {
                    if row.state == "missing" {
                        // Already classified in an earlier recovery and the
                        // file is still absent: repeating the gap/action
                        // would break idempotence; state and gaps are
                        // already correct.
                    } else {
                        store
                            .add_log_gap(terminal_id, row.first_line, row.last_line, "missing")
                            .await?;
                        store
                            .repair_segment(
                                row.segment_id,
                                row.committed_bytes,
                                row.fsynced_bytes,
                                row.last_line,
                                "missing",
                            )
                            .await?;
                        degraded_set = true;
                        actions.push(format!(
                            "missing segment {}: log_gap lines [{}, {})",
                            row.segment_id, row.first_line, row.last_line
                        ));
                    }
                }
                SegmentClass::EpochMismatch => unreachable!("handled above"),
                SegmentClass::Corrupt | SegmentClass::Truncated => {
                    let reason = if ex.class == SegmentClass::Truncated {
                        "truncated"
                    } else {
                        "corrupt"
                    };
                    if ex.quarantine_whole || ex.last_good_line.is_none() {
                        let moved = quarantine_file(root, &row.file_name, store.crash())?;
                        store
                            .add_log_gap(terminal_id, row.first_line, row.last_line, reason)
                            .await?;
                        store
                            .repair_segment(row.segment_id, 0, 0, row.first_line, "quarantined")
                            .await?;
                        actions.push(format!(
                            "{reason} segment {} ({}): file quarantined as quarantine/{moved}; log_gap lines [{}, {})",
                            row.segment_id, ex.detail, row.first_line, row.last_line
                        ));
                    } else {
                        let data = read_bounded(&root.join(&row.file_name))?;
                        let keep = ex.valid_end as usize;
                        let artifact = quarantine_tail_bytes(
                            root,
                            &row.file_name,
                            ex.valid_end,
                            &data[keep..],
                            store.crash(),
                        )?;
                        let new_last = ex.last_good_line.unwrap() + 1;
                        store
                            .add_log_gap(terminal_id, new_last, row.last_line, reason)
                            .await?;
                        store
                            .repair_segment(
                                row.segment_id,
                                ex.valid_end,
                                ex.valid_end,
                                new_last,
                                &row.state,
                            )
                            .await?;
                        actions.push(format!(
                            "{reason} inside committed region of segment {} ({}): truncated to {keep} bytes, artifact quarantine/{artifact}; available range [{}, {}); log_gap lines [{}, {})",
                            row.segment_id, ex.detail, row.first_line, new_last, new_last, row.last_line
                        ));
                    }
                    degraded_set = true;
                }
                SegmentClass::Partial => {
                    // Uncommitted complete frames or a bad tail: truncate to
                    // the committed boundary, quarantine the bytes, never adopt.
                    let data = read_bounded(&root.join(&row.file_name))?;
                    let keep = row.committed_bytes.min(data.len() as u64) as usize;
                    let artifact = quarantine_tail_bytes(
                        root,
                        &row.file_name,
                        keep as u64,
                        &data[keep..],
                        store.crash(),
                    )?;
                    actions.push(format!(
                        "uncommitted tail of segment {} ({}): truncated to {keep} bytes, artifact quarantine/{artifact}; frames never adopted",
                        row.segment_id, ex.detail
                    ));
                }
                SegmentClass::Pending => {
                    // Zero committed bytes: the whole file is unindexed
                    // (header only, or header plus frames whose transaction
                    // never committed). Quarantine the file, tombstone the
                    // row, fabricate neither a gap nor continuity.
                    if root.join(&row.file_name).exists() {
                        let moved = quarantine_file(root, &row.file_name, store.crash())?;
                        actions.push(format!(
                            "pending segment {} ({}): file quarantined as quarantine/{moved}; row tombstoned; no gap fabricated",
                            row.segment_id, ex.detail
                        ));
                    } else {
                        actions.push(format!(
                            "pending segment {} ({}): no file; row tombstoned",
                            row.segment_id, ex.detail
                        ));
                    }
                    store
                        .repair_segment(row.segment_id, 0, 0, row.first_line, "quarantined")
                        .await?;
                }
                SegmentClass::Orphan => unreachable!("orphans come from the directory sweep"),
            }
        }
    }

    // Directory sweep: segment files that no DB row owns (or whose row is a
    // quarantine tombstone) are detectable and get quarantined, never adopted.
    let mut orphan_found = false;
    for entry in std::fs::read_dir(root).context("read storage root")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_segment_file_name(&name) || !entry.file_type()?.is_file() {
            continue;
        }
        let owned = match store.segment_by_file(&name).await? {
            Some(row) => row.state != "quarantined",
            None => false,
        };
        if owned {
            continue;
        }
        orphan_found = true;
        let moved = quarantine_file(root, &name, store.crash())?;
        outcomes.push(SegmentOutcome {
            segment_id: None,
            file_name: name.clone(),
            class: SegmentClass::Orphan,
            detail: "segment file present in directory but not owned by the DB".into(),
        });
        actions.push(format!(
            "orphan segment file {name} quarantined as quarantine/{moved}"
        ));
    }

    if created_terminal && orphan_found {
        destructive_rebuild = true;
        actions.push(format!(
            "db missing/replaced: terminal recreated with fresh epoch {}; old segment files quarantined; old cursors rejected",
            Uuid::from_bytes(epoch)
        ));
    }

    if degraded_set {
        store.set_degraded(terminal_id).await?;
    }

    let term = store.get_terminal(terminal_id).await?;
    let gaps = store.list_gaps(terminal_id).await?;
    Ok(RecoveryReport {
        created_terminal,
        epoch,
        epoch_rotated: epoch_rotated || created_terminal,
        destructive_rebuild,
        outcomes,
        actions,
        gaps,
        degraded: term.degraded,
        line_watermark: term.line_watermark,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::NewSegment;
    use crate::frame::{
        FRAME_FLAG_LINE_END, FRAME_KIND_DATA, FrameHeader, SEGMENT_KIND_DATA, encode_frame,
    };
    use std::path::PathBuf;

    fn test_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ql-storage-rec-{}-{}",
            tag,
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn migrations() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
    }

    fn frame(seq: u64, line: u64, offset: u64, payload: &[u8]) -> Vec<u8> {
        encode_frame(
            &FrameHeader {
                kind: FRAME_KIND_DATA,
                flags: FRAME_FLAG_LINE_END,
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: payload.len() as u32,
            },
            payload,
        )
        .unwrap()
    }

    fn segment_file_header(row_id: i64, uuid: [u8; 16], epoch: [u8; 16]) -> Vec<u8> {
        SegmentHeader {
            kind: SEGMENT_KIND_DATA,
            flags: 0,
            terminal: uuid,
            epoch,
            segment_id: row_id as u64,
            created_ms: 42,
        }
        .encode()
        .to_vec()
    }

    /// Build a store with one terminal and return (root, store, segment row,
    /// header, frames already appended+committed to disk and DB).
    async fn fixture(tag: &str) -> (PathBuf, Store, SegmentRow, [u8; 16], [u8; 16]) {
        let root = test_root(tag);
        let store = Store::open(&root.join("terminal.db"), &migrations())
            .await
            .unwrap();
        let uuid = [7u8; 16];
        let epoch = [9u8; 16];
        store.create_terminal("t", uuid, epoch).await.unwrap();
        let seg_id = store
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        let mut data = segment_file_header(seg_id, uuid, epoch);
        data.extend_from_slice(&frame(0, 1, 0, b"one"));
        data.extend_from_slice(&frame(1, 2, 0, b"two"));
        data.extend_from_slice(&frame(2, 3, 0, b"three"));
        std::fs::write(root.join("seg-000001.log"), &data).unwrap();
        store
            .commit_visible(
                "t",
                &crate::db::CommitInput {
                    segment_id: seg_id,
                    committed_bytes: data.len() as u64,
                    fsynced_bytes: data.len() as u64,
                    segment_last_line: 4,
                    line_watermark: 3,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let row = store.get_segment(seg_id).await.unwrap();
        (root, store, row, uuid, epoch)
    }

    fn outcomes_by_class(report: &RecoveryReport, class: SegmentClass) -> usize {
        report.outcomes.iter().filter(|o| o.class == class).count()
    }

    #[tokio::test]
    async fn clean_state_stays_clean_and_idempotent() {
        let (root, store, _row, _u, _e) = fixture("clean").await;
        let r1 = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&r1, SegmentClass::Clean), 1, "{r1:?}");
        assert!(r1.actions.is_empty());
        assert!(!r1.degraded);
        assert_eq!(r1.line_watermark, 3);
        let r2 = recover(&store, &root, "t").await.unwrap();
        assert_eq!(r1.outcomes, r2.outcomes);
        assert_eq!(r1.gaps, r2.gaps);
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn uncommitted_tail_truncated_and_quarantined() {
        let (root, store, row, _u, _e) = fixture("uncommitted").await;
        let committed = row.committed_bytes as usize;
        let mut data = std::fs::read(root.join("seg-000001.log")).unwrap();
        data.extend_from_slice(&frame(3, 4, 0, b"ghost"));
        std::fs::write(root.join("seg-000001.log"), &data).unwrap();

        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&report, SegmentClass::Partial), 1);
        let after = std::fs::read(root.join("seg-000001.log")).unwrap();
        assert_eq!(
            after.len(),
            committed,
            "truncated back to committed boundary"
        );
        assert_eq!(
            store
                .get_segment(row.segment_id)
                .await
                .unwrap()
                .committed_bytes,
            committed as u64
        );
        assert_eq!(
            store.get_terminal("t").await.unwrap().line_watermark,
            3,
            "watermark untouched"
        );
        assert!(
            report.gaps.is_empty(),
            "no gap for data that was never committed"
        );

        // artifact exists in quarantine; second recovery changes nothing.
        assert!(
            !std::fs::read_dir(root.join(QUARANTINE_DIR))
                .unwrap()
                .next()
                .is_none()
        );
        let again = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&again, SegmentClass::Clean), 1);
        assert_eq!(
            std::fs::read(root.join("seg-000001.log")).unwrap().len(),
            committed
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn bad_tail_half_frame_truncated() {
        let (root, store, row, _u, _e) = fixture("half").await;
        let committed = row.committed_bytes as usize;
        let mut data = std::fs::read(root.join("seg-000001.log")).unwrap();
        data.extend_from_slice(&frame(3, 4, 0, b"half-frame")[..20]); // cut mid-payload
        std::fs::write(root.join("seg-000001.log"), &data).unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&report, SegmentClass::Partial), 1);
        assert_eq!(
            std::fs::read(root.join("seg-000001.log")).unwrap().len(),
            committed
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn corrupt_committed_frame_creates_gap_reduces_range_not_watermark() {
        let (root, store, row, _u, _e) = fixture("corrupt").await;
        // Corrupt the payload of the 3rd frame (line 3).
        let mut data = std::fs::read(root.join("seg-000001.log")).unwrap();
        let scan = scan_frames(&data, SEGMENT_HEADER_LEN);
        assert_eq!(scan.outcome, ScanOutcome::Clean);
        let third = &scan.frames[2];
        data[third.payload_at] ^= 0xFF;
        std::fs::write(root.join("seg-000001.log"), &data).unwrap();

        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(
            outcomes_by_class(&report, SegmentClass::Corrupt),
            1,
            "{report:?}"
        );
        let gaps = store.list_gaps("t").await.unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!((gaps[0].first_line, gaps[0].last_line), (3, 4));
        assert_eq!(gaps[0].reason, "corrupt");
        let seg = store.get_segment(row.segment_id).await.unwrap();
        assert_eq!(seg.last_line, 3, "available range reduced");
        assert_eq!(seg.committed_bytes, scan.frames[1].end as u64);
        assert_eq!(
            store.get_terminal("t").await.unwrap().line_watermark,
            3,
            "watermark never lowered"
        );
        assert!(store.get_terminal("t").await.unwrap().degraded);
        assert_eq!(
            std::fs::read(root.join("seg-000001.log")).unwrap().len(),
            scan.frames[1].end
        );

        // Idempotent: second pass is clean with the same gaps.
        let again = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&again, SegmentClass::Clean), 1);
        assert_eq!(store.list_gaps("t").await.unwrap().len(), 1);
        assert_eq!(
            std::fs::read(root.join("seg-000001.log")).unwrap().len(),
            scan.frames[1].end
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn truncated_committed_file_creates_gap() {
        let (root, store, row, _u, _e) = fixture("truncated").await;
        let data = std::fs::read(root.join("seg-000001.log")).unwrap();
        let scan = scan_frames(&data, SEGMENT_HEADER_LEN);
        let cut = scan.frames[1].end + 5; // inside the third frame's header
        truncate_file(&root.join("seg-000001.log"), cut as u64).unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(
            outcomes_by_class(&report, SegmentClass::Truncated),
            1,
            "{report:?}"
        );
        let gaps = store.list_gaps("t").await.unwrap();
        assert_eq!(
            (
                gaps[0].first_line,
                gaps[0].last_line,
                gaps[0].reason.as_str()
            ),
            (3, 4, "truncated")
        );
        assert_eq!(store.get_terminal("t").await.unwrap().line_watermark, 3);
        assert_eq!(
            store.get_segment(row.segment_id).await.unwrap().last_line,
            3
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn missing_segment_file_creates_gap_and_degrades() {
        let (root, store, _row, _u, _e) = fixture("missing-file").await;
        std::fs::remove_file(root.join("seg-000001.log")).unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&report, SegmentClass::Missing), 1);
        let gaps = store.list_gaps("t").await.unwrap();
        assert_eq!(
            (
                gaps[0].first_line,
                gaps[0].last_line,
                gaps[0].reason.as_str()
            ),
            (1, 4, "missing")
        );
        assert_eq!(
            store.get_terminal("t").await.unwrap().line_watermark,
            3,
            "watermark preserved"
        );
        assert!(store.get_terminal("t").await.unwrap().degraded);
        // idempotent
        let again = recover(&store, &root, "t").await.unwrap();
        assert_eq!(again.gaps.len(), 1);
        assert_eq!(outcomes_by_class(&again, SegmentClass::Missing), 1);
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn orphan_directory_files_quarantined() {
        let (root, store, _row, uuid, epoch) = fixture("orphan").await;
        let mut stray = segment_file_header(99, uuid, epoch);
        stray.extend_from_slice(&frame(0, 99, 0, b"stray"));
        std::fs::write(root.join("seg-000099.log"), &stray).unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(
            outcomes_by_class(&report, SegmentClass::Orphan),
            1,
            "{report:?}"
        );
        assert!(
            !root.join("seg-000099.log").exists(),
            "moved out of the live directory"
        );
        assert!(
            store
                .segment_by_file("seg-000099.log")
                .await
                .unwrap()
                .is_none(),
            "never adopted"
        );
        // idempotent: the sweep has nothing left to do
        let again = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&again, SegmentClass::Orphan), 0);
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn epoch_mismatch_triggers_destructive_rebuild() {
        let (root, store, _row, _u, _e) = fixture("epoch").await;
        let replaced = uuid::Uuid::now_v7().into_bytes();
        // Simulate a replaced DB: the terminal row now carries a foreign epoch.
        sqlx::query("UPDATE terminal SET log_epoch = ?1 WHERE terminal_id = 't'")
            .bind(replaced.as_slice())
            .execute(store.pool())
            .await
            .unwrap();

        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(outcomes_by_class(&report, SegmentClass::EpochMismatch), 1);
        assert!(report.destructive_rebuild);
        assert_ne!(report.epoch, [9u8; 16]);
        let term = store.get_terminal("t").await.unwrap();
        assert_eq!(term.log_epoch, report.epoch);
        assert_eq!(term.line_watermark, 3, "watermark preserved across rebuild");
        assert_eq!(term.active_segment, None);
        assert!(store.segments("t").await.unwrap().is_empty());
        assert!(
            !root.join("seg-000001.log").exists(),
            "old file quarantined"
        );
        // old-epoch cursor rejected
        assert!(term.validate_cursor(&[9; 16], 1, (1, 0)).is_err());
        // idempotent
        let again = recover(&store, &root, "t").await.unwrap();
        assert!(again.outcomes.is_empty());
        assert!(!again.destructive_rebuild);
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn db_missing_is_destructive_rebuild() {
        let (root, store, _row, _uuid, epoch) = fixture("dbmissing").await;
        store.close().await;
        std::fs::remove_file(root.join("terminal.db")).unwrap();
        std::fs::remove_file(root.join("terminal.db-wal")).ok();
        std::fs::remove_file(root.join("terminal.db-shm")).ok();
        // Old files remain in the directory while the DB is gone.
        assert!(root.join("seg-000001.log").exists());

        let store = Store::open(&root.join("terminal.db"), &migrations())
            .await
            .unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert!(report.created_terminal);
        assert!(
            report.destructive_rebuild,
            "orphaned old files must force a rebuild decision"
        );
        assert_ne!(report.epoch, epoch);
        assert!(!root.join("seg-000001.log").exists());
        assert!(store.segments("t").await.unwrap().is_empty());
        let term = store.get_terminal("t").await.unwrap();
        assert!(
            term.validate_cursor(&epoch, 1, (1, 0)).is_err(),
            "old cursor rejected"
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn oversized_segment_header_corruption_quarantines_file() {
        let (root, store, _row, _u, _e) = fixture("badheader").await;
        let mut data = std::fs::read(root.join("seg-000001.log")).unwrap();
        data[0] = b'X'; // break magic
        std::fs::write(root.join("seg-000001.log"), &data).unwrap();
        let report = recover(&store, &root, "t").await.unwrap();
        assert_eq!(
            outcomes_by_class(&report, SegmentClass::Corrupt),
            1,
            "{report:?}"
        );
        assert!(!root.join("seg-000001.log").exists());
        let gaps = store.list_gaps("t").await.unwrap();
        assert_eq!((gaps[0].first_line, gaps[0].last_line), (1, 4));
        assert_eq!(store.get_terminal("t").await.unwrap().line_watermark, 3);
        // idempotent: tombstone row skipped
        let again = recover(&store, &root, "t").await.unwrap();
        assert_eq!(again.outcomes.len(), 0);
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }
}
