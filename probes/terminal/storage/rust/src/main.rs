//! Throwaway hybrid storage probe (probe C, Gate C). NOT production code; it
//! validates the commit hypothesis from docs/design
//! terminal-technical-validation-plan.md §6:
//!
//!   1. append complete frames to the segment file;
//!   2. `sync_data` the file (and on first creation the header plus the
//!      parent directory — the directory fsync is mandatory);
//!   3. only then run the short SQLite transaction that makes the lines
//!      visible (segment/terminal/event/exit in one transaction);
//!   4. only after that transaction commits publish the lines.
//!
//! Gate C runs the crash matrix across REAL child processes: `writer`
//! children die with `libc::_exit(70)` at exact statement boundaries
//! (process-crash evidence), the gate harness additionally truncates files to
//! their fsynced checkpoint for power-loss evidence, and `recover` always
//! runs in a NEW process.

mod crash;
mod db;
mod frame;
mod gate;
mod harness;
mod reader;
mod recovery;

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use db::{CommitInput, NewSegment, NewTailRevision, SegmentRow, Store};
use frame::{
    FRAME_FLAG_LINE_END, FRAME_KIND_DATA, FrameHeader, SEGMENT_HEADER_LEN, SEGMENT_KIND_DATA,
    SegmentHeader, encode_frame, scan_frames, split_line,
};
use serde::Serialize;
use uuid::Uuid;

use crate::crash::REFUSE_EXIT;

/// Probe-local rotation threshold; production granularity is a Gate C topic.
pub const MAX_SEGMENT_BYTES: u64 = 256 * 1024;

pub const DB_FILE: &str = "terminal.db";
pub const TERMINAL_ID: &str = "probe-terminal";

/// Bounded drain memory: the fake producer may buffer at most 8 pending
/// frames (counting every frame of a multi-frame line) and at most
/// `DRAIN_QUEUE_FRAMES * MAX_PAYLOAD` payload bytes while writer-side I/O
/// is failing. Queued lines are the actual produced payload objects, so
/// the bound is a real bound on buffered producer memory.
pub const DRAIN_QUEUE_FRAMES: usize = 8;
pub const DRAIN_QUEUE_BYTES: usize = DRAIN_QUEUE_FRAMES * frame::MAX_PAYLOAD as usize;

/// Crash checkpoints inside recovery directory mutations (recover children
/// die at these exact boundaries; see quarantine_file/quarantine_tail_bytes).
pub const RECOVERY_CRASH_POINTS: &[&str] = &[
    "rec_artifact_written",
    "rec_tail_truncated",
    "rec_file_quarantined",
];

/// Every crash point the matrix covers (writer + store hooks).
pub const CRASH_POINTS: &[&str] = &[
    // segment creation
    "seg_before",
    "seg_header_written",
    "seg_header_synced",
    "seg_before_db_row",
    // frame append
    "frame_before_write",
    "frame_mid_write",
    "frame_after_write",
    "frame_after_sync",
    // SQLite transaction (db.rs)
    "txn_begin",
    "txn_update",
    "txn_before_commit",
    "txn_after_commit",
    "event_insert",
    "event_commit",
    // publication
    "publish_before",
    "publish_after",
    "event_publish",
];

/// Deterministic line content so the harness can recompute every line.
pub fn line_content(line: u64, bytes: usize) -> String {
    let marker = format!("line-{line:05}-eol");
    let pad = bytes.saturating_sub(marker.len());
    format!("{marker}{}", "x".repeat(pad))
}

/// fsync a directory so a newly created entry (segment file) is durable.
pub fn fsync_dir(path: &Path) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("cstring for {}", path.display()))?;
    // SAFETY: `c` outlives the call; open/fsync/close only touch the given fd.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        bail!(
            "open dir {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    let r = unsafe { libc::fsync(fd) };
    let err = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if r != 0 {
        bail!("fsync dir {}: {err}", path.display());
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A line published only after its SQLite transaction committed.
#[derive(Debug, Clone, Serialize)]
pub struct PublishedLine {
    pub epoch: String,
    pub line: u64,
    pub bytes: usize,
    pub segment_id: i64,
    pub frames: usize,
    /// Event sequence assigned inside the commit transaction (present only
    /// when this line carried an event).
    pub event_seq: Option<u64>,
}

/// Injected writer-side I/O failure for the drain scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultStep {
    Append,
    Sync,
    Commit,
}

impl FaultStep {
    fn parse(s: &str) -> Result<FaultStep> {
        match s {
            "append" => Ok(FaultStep::Append),
            "sync" => Ok(FaultStep::Sync),
            "commit" => Ok(FaultStep::Commit),
            other => bail!("unknown drain step {other}"),
        }
    }
}

#[derive(Debug)]
pub struct Fault {
    pub step: FaultStep,
    pub remaining: u32,
}

struct ActiveSegment {
    row: SegmentRow,
    file: std::fs::File,
    /// Current file length; equals the committed boundary after every commit.
    len: u64,
}

/// A produced line buffered in the drain queue: the real payload is held
/// here (never regenerated at flush time), so the queue bounds are bounds
/// on actual buffered producer memory.
struct PendingLine {
    index: u64,
    content: String,
    event_payload: String,
}

pub struct Writer {
    store: Arc<Store>,
    root: PathBuf,
    terminal_id: String,
    terminal_uuid: [u8; 16],
    epoch: [u8; 16],
    watermark: u64,
    active: Option<ActiveSegment>,
    next_frame_seq: u64,
    max_segment_bytes: u64,
    fault: Option<Fault>,
    /// Park the writer between `sync_data` and the SQLite commit until this
    /// file appears (commit-before-query concurrency evidence).
    pre_commit_gate: Option<PathBuf>,
}

impl Writer {
    /// Open for appending. Recovery always runs first: the writer only ever
    /// operates on a recovered, unambiguous state.
    pub async fn open(store: Arc<Store>, root: &Path, terminal_id: &str) -> Result<Writer> {
        Writer::open_with(store, root, terminal_id, MAX_SEGMENT_BYTES).await
    }

    /// `open` with a probe-local rotation threshold (small segments force
    /// >=3 rotations cheaply).
    pub async fn open_with(
        store: Arc<Store>,
        root: &Path,
        terminal_id: &str,
        max_segment_bytes: u64,
    ) -> Result<Writer> {
        let report = recovery::recover(&store, root, terminal_id).await?;
        if report.degraded {
            // Foundation policy: appending to a degraded log is allowed (the
            // probe keeps recording), but the state must stay observable.
            eprintln!("recovery: degraded=1 watermark={}", report.line_watermark);
        }
        let term = store.get_terminal(terminal_id).await?;
        if term.refuse_new_start {
            bail!(
                "REFUSE-NEW-START terminal {terminal_id} latched a drain overflow \
                 (explicit missing range recorded); a destructive rebuild is \
                 required before a new writer start"
            );
        }
        let mut writer = Writer {
            store,
            root: root.to_path_buf(),
            terminal_id: terminal_id.to_string(),
            terminal_uuid: term.terminal_uuid,
            epoch: term.log_epoch,
            watermark: term.line_watermark,
            active: None,
            next_frame_seq: 0,
            max_segment_bytes,
            fault: None,
            pre_commit_gate: None,
        };
        writer.attach_active_segment().await?;
        Ok(writer)
    }

    /// Attach the active segment row (if any) and rescan the committed prefix
    /// to recover the frame sequence position. The file length must equal the
    /// committed boundary after recovery.
    async fn attach_active_segment(&mut self) -> Result<()> {
        let term = self.store.get_terminal(&self.terminal_id).await?;
        let Some(segment_id) = term.active_segment else {
            return Ok(());
        };
        let row = self.store.get_segment(segment_id).await?;
        let path = self.root.join(&row.file_name);
        if row.state != "active" || !path.exists() {
            return Ok(()); // sealed/quarantined/missing: start a new segment
        }
        let data = std::fs::read(&path)?;
        if (data.len() as u64) != row.committed_bytes {
            bail!(
                "post-recovery invariant broken for {}: file {} != committed {}",
                row.file_name,
                data.len(),
                row.committed_bytes
            );
        }
        let scan = scan_frames(&data, SEGMENT_HEADER_LEN);
        if scan.outcome != frame::ScanOutcome::Clean {
            bail!("post-recovery scan of {} not clean", row.file_name);
        }
        self.next_frame_seq = scan
            .frames
            .last()
            .map(|f| f.header.frame_seq + 1)
            .unwrap_or(0);
        let len = data.len() as u64;
        let file = std::fs::OpenOptions::new()
            .append(true)
            .read(true)
            .open(&path)?;
        self.active = Some(ActiveSegment { row, file, len });
        Ok(())
    }

    pub fn set_fault(&mut self, fault: Fault) {
        self.fault = Some(fault);
    }

    pub fn set_pre_commit_gate(&mut self, gate: PathBuf) {
        self.pre_commit_gate = Some(gate);
    }

    /// Consume one injected failure at `step` (returns true when injected).
    fn fail_at(&mut self, step: FaultStep) -> bool {
        if let Some(f) = &mut self.fault {
            if f.step == step && f.remaining > 0 {
                f.remaining -= 1;
                return true;
            }
        }
        false
    }

    /// Abandon the active segment after an injected I/O failure: any bytes
    /// beyond the committed boundary stay on disk for recovery to truncate
    /// or quarantine; they are never adopted by this writer.
    fn abandon_active(&mut self) {
        if self.active.take().is_some() {
            self.store.crash().step("active_segment_abandoned");
        }
    }

    /// Append one normalized UTF-8 line and publish it. Long lines are split
    /// into frames only on character boundaries; a repeated line number
    /// always carries an increasing offset.
    pub async fn append_line(
        &mut self,
        text: &str,
        event: Option<(&'static str, String)>,
        exit: Option<(i64, &'static str)>,
    ) -> Result<PublishedLine> {
        let line = self.watermark + 1;
        self.append_line_numbered(line, text, event, exit).await
    }

    /// Append with an explicit line number (drain mode reserves numbers for
    /// dropped lines; numbering stays strictly increasing and never reuses a
    /// number, leaving explicit holes recorded as log_gap).
    pub async fn append_line_numbered(
        &mut self,
        line: u64,
        text: &str,
        event: Option<(&'static str, String)>,
        exit: Option<(i64, &'static str)>,
    ) -> Result<PublishedLine> {
        if line <= self.watermark {
            bail!(
                "line {line} would reuse or lower watermark {}",
                self.watermark
            );
        }
        let chunks = split_line(text);
        // Encoded size only (per frame: header + payload + crc32): the
        // rotation decision needs it BEFORE the frames are built, because a
        // rotation resets `next_frame_seq` to 0 and the frame sequence must
        // be derived from the segment that actually receives the bytes.
        let frame_overhead = (crate::frame::FRAME_HEADER_LEN + 4) as u64;
        let buf_len: u64 = chunks.iter().map(|c| c.len() as u64 + frame_overhead).sum();
        // The exact end-of-line byte offset: the fixed cursor of this
        // line's tail revision (NOT the last chunk's start, which would
        // land inside a multi-frame line).
        let line_end_offset = text.len() as u64;

        // Logs cleared while running: the process must survive, keep the
        // terminal record and line numbering, and roll to a new segment.
        if let Some(a) = self.active.as_ref() {
            if !self.root.join(&a.row.file_name).exists() {
                self.store.crash().step("active_segment_vanished");
                self.active = None;
            }
        }

        // Rotation before writing if this line would overflow the segment.
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.row.state == "active" && a.len > SEGMENT_HEADER_LEN as u64)
            && self.active.as_ref().unwrap().len + buf_len > self.max_segment_bytes
        {
            self.seal_active().await?;
        }
        if self.active.is_none() {
            self.create_segment(line).await?;
        }

        // Build the frames only now, with the receiving segment's frame
        // sequence (a fresh segment starts at 0; the scanner requires it).
        let mut buf = Vec::new();
        let mut seq = self.next_frame_seq;
        let mut offset = 0u64;
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i + 1 == chunks.len();
            let header = FrameHeader {
                kind: FRAME_KIND_DATA,
                flags: if is_last { FRAME_FLAG_LINE_END } else { 0 },
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: chunk.len() as u32,
            };
            buf.extend_from_slice(&encode_frame(&header, chunk)?);
            offset += chunk.len() as u64;
            seq += 1;
        }

        // 1. append complete frames
        self.store.crash().hit("frame_before_write");
        if self.fail_at(FaultStep::Append) {
            self.abandon_active();
            bail!("injected append failure");
        }
        if self.store.crash().point() == Some("frame_mid_write") {
            // Die with half the buffer on disk (a torn write).
            let active = self.active.as_mut().context("active segment")?;
            let half = buf.len() / 2;
            active
                .file
                .write_all(&buf[..half])
                .with_context(|| format!("torn append to {}", active.row.file_name))?;
            self.store.crash().hit("frame_mid_write");
            unreachable!("frame_mid_write exits");
        }
        {
            let active = self.active.as_mut().context("active segment")?;
            active
                .file
                .write_all(&buf)
                .with_context(|| format!("append to {}", active.row.file_name))?;
        }
        self.store.crash().hit("frame_after_write");
        // 2. sync file data (header + parent dir were already synced at creation)
        if self.fail_at(FaultStep::Sync) {
            self.abandon_active();
            bail!("injected sync failure");
        }
        self.active
            .as_mut()
            .context("active segment")?
            .file
            .sync_data()?;
        self.store.crash().hit("frame_after_sync");
        if self.fail_at(FaultStep::Commit) {
            self.abandon_active();
            bail!("injected commit failure");
        }
        // Optional concurrency gate: park between fsync and the commit
        // transaction so a concurrent reader can prove the appended bytes
        // stay invisible until the transaction commits.
        if let Some(gate) = self.pre_commit_gate.clone() {
            self.store.crash().step("pre_commit_gate_wait_begin");
            wait_for_gate_file(&gate)?;
            self.store.crash().step("pre_commit_gate_released");
        }

        let new_len = self.active.as_ref().context("active segment")?.len + buf.len() as u64;
        let segment_id = self
            .active
            .as_ref()
            .context("active segment")?
            .row
            .segment_id;
        // 3. short SQLite transaction publishing the state
        let commit = CommitInput {
            segment_id,
            committed_bytes: new_len,
            fsynced_bytes: new_len,
            segment_last_line: line + 1,
            line_watermark: line,
            tail: Some(NewTailRevision {
                revision: line,
                line,
                byte_offset: line_end_offset,
                segment_id,
            }),
            event,
            exit,
        };
        let event_seq = self
            .store
            .commit_visible(&self.terminal_id, &commit)
            .await?;
        self.store.crash().hit("publish_before");
        // 4. publish only now
        if let Some(active) = self.active.as_mut() {
            active.len = new_len;
            active.row.committed_bytes = new_len;
            active.row.fsynced_bytes = new_len;
            active.row.last_line = line + 1;
        }
        self.watermark = line;
        self.next_frame_seq = seq;
        Ok(PublishedLine {
            epoch: Uuid::from_bytes(self.epoch).to_string(),
            line,
            bytes: text.len(),
            segment_id,
            frames: chunks.len(),
            event_seq,
        })
    }

    /// Fixture helper: append + sync the file but never run the SQLite
    /// transaction, simulating a writer killed between fsync and commit.
    pub fn append_uncommitted_for_fixture(&mut self, text: &str) -> Result<u64> {
        let line = self.watermark + 1;
        let chunks = split_line(text);
        let mut buf = Vec::new();
        let mut seq = self.next_frame_seq;
        let mut offset = 0u64;
        for (i, chunk) in chunks.iter().enumerate() {
            let header = FrameHeader {
                kind: FRAME_KIND_DATA,
                flags: if i + 1 == chunks.len() {
                    FRAME_FLAG_LINE_END
                } else {
                    0
                },
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: chunk.len() as u32,
            };
            buf.extend_from_slice(&encode_frame(&header, chunk)?);
            offset += chunk.len() as u64;
            seq += 1;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.len + buf.len() as u64 > self.max_segment_bytes)
        {
            bail!("fixture overflow: rotate first");
        }
        let active = self.active.as_mut().context("active segment")?;
        active.file.write_all(&buf)?;
        active.file.sync_data()?;
        Ok(line)
    }

    async fn seal_active(&mut self) -> Result<()> {
        if let Some(a) = self.active.take() {
            a.file.sync_data()?;
            self.store.seal_segment(a.row.segment_id).await?;
        }
        Ok(())
    }

    /// Create a fresh segment file: row first (for the id the header must
    /// carry), then the file, then sync the header and the parent directory
    /// before any frame is written. The directory fsync after first creation
    /// is mandatory (trace step `seg_dir_fsynced`).
    async fn create_segment(&mut self, first_line: u64) -> Result<()> {
        self.store.crash().hit("seg_before");
        if self.store.crash().point() == Some("seg_before_db_row") {
            // Variant that models the writer dying BEFORE the DB row exists:
            // a durable, fully synced segment file that no row owns. Recovery
            // must discover and quarantine it, never adopt it.
            let id = self.store.next_segment_id().await?;
            let file_name = format!("seg-{id:06}.log");
            let header = SegmentHeader {
                kind: SEGMENT_KIND_DATA,
                flags: 0,
                terminal: self.terminal_uuid,
                epoch: self.epoch,
                segment_id: id as u64,
                created_ms: now_ms(),
            };
            let path = self.root.join(&file_name);
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&path)
                .with_context(|| format!("create {}", path.display()))?;
            file.write_all(&header.encode())?;
            file.sync_data()?;
            fsync_dir(&self.root)?;
            self.store.crash().step("seg_dir_fsynced");
            self.store.crash().hit("seg_before_db_row");
            unreachable!("seg_before_db_row exits");
        }
        let file_name = format!("seg-{:06}.log", self.store.next_segment_id().await?);
        let segment_id = self
            .store
            .insert_segment(&NewSegment {
                terminal_id: self.terminal_id.clone(),
                file_name: file_name.clone(),
                first_line,
                last_line: first_line,
            })
            .await?;
        self.store.crash().step("seg_row_inserted");
        let header = SegmentHeader {
            kind: SEGMENT_KIND_DATA,
            flags: 0,
            terminal: self.terminal_uuid,
            epoch: self.epoch,
            segment_id: segment_id as u64,
            created_ms: now_ms(),
        };
        let path = self.root.join(&file_name);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(&header.encode())?;
        self.store.crash().hit("seg_header_written");
        file.sync_data()?;
        fsync_dir(&self.root)?;
        self.store.crash().step("seg_dir_fsynced");
        self.store.crash().hit("seg_header_synced");
        let row = self.store.get_segment(segment_id).await?;
        self.active = Some(ActiveSegment {
            row,
            file,
            len: SEGMENT_HEADER_LEN as u64,
        });
        self.next_frame_seq = 0;
        Ok(())
    }

    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    pub fn epoch(&self) -> [u8; 16] {
        self.epoch
    }
}

fn emit_json(v: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn published_record(p: &PublishedLine) -> serde_json::Value {
    serde_json::json!({ "published": {
        "epoch": p.epoch,
        "line": p.line,
        "bytes": p.bytes,
        "segment_id": p.segment_id,
        "frames": p.frames,
        "event_seq": p.event_seq,
    }})
}

#[derive(Debug, Default)]
struct WriterArgs {
    workdir: Option<PathBuf>,
    migrations: Option<PathBuf>,
    terminal: Option<String>,
    max_segment_bytes: u64,
    crash: Option<String>,
    trace: Option<PathBuf>,
    prelude: usize,
    append: usize,
    line_bytes: usize,
    long_utf8: usize,
    event_per_line: bool,
    with_exit: Option<i64>,
    drain_step: Option<String>,
    drain_fail: u32,
    producer_lines: usize,
    gate_after_line: Option<u64>,
    gate_file: Option<PathBuf>,
    pre_commit_gate: Option<PathBuf>,
}

fn wait_for_gate_file(path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !path.exists() {
        if std::time::Instant::now() > deadline {
            bail!("gate file {} never appeared", path.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Ok(())
}

fn wait_for_gate(path: &Path, ctl: &crash::CrashCtl) -> Result<()> {
    ctl.step("gate_wait_begin");
    wait_for_gate_file(path)?;
    ctl.step("gate_released");
    Ok(())
}

/// One scripted append; publishes to stdout only after the commit.
async fn append_one(
    writer: &mut Writer,
    ctl: &crash::CrashCtl,
    args: &WriterArgs,
    content: &str,
    exit: Option<(i64, &'static str)>,
) -> Result<()> {
    let event = if exit.is_some() {
        Some(("exit", format!(r#"{{"code":{}}}"#, exit.unwrap().0)))
    } else if args.event_per_line {
        Some((
            "output",
            format!(r#"{{"line":{}}}"#, writer.watermark() + 1),
        ))
    } else {
        None
    };
    let published = writer.append_line(content, event, exit).await?;
    emit_json(&published_record(&published));
    ctl.hit("publish_after");
    if let Some(seq) = published.event_seq {
        emit_json(
            &serde_json::json!({ "event_published": { "seq": seq, "line": published.line } }),
        );
        ctl.hit("event_publish");
    }
    if args.gate_after_line == Some(published.line) {
        let gate = args
            .gate_file
            .as_ref()
            .context("--gate-after-line requires --gate-file")?;
        wait_for_gate(gate, ctl)?;
    }
    Ok(())
}

/// Drain scenario: a fake producer keeps producing across injected
/// append/sync/commit failures, buffered as the ACTUAL produced payload
/// objects under an explicit frame bound (`DRAIN_QUEUE_FRAMES`, counting
/// every frame of a multi-frame line) and byte bound (`DRAIN_QUEUE_BYTES`);
/// overflow drops are recorded as an explicit log_gap, the terminal is
/// marked degraded and refuses new writer starts.
async fn run_drain(writer: &mut Writer, store: &Arc<Store>, args: &WriterArgs) -> Result<()> {
    let step = FaultStep::parse(
        args.drain_step
            .as_deref()
            .context("--drain-step is required for the drain scenario")?,
    )?;
    let k = args.producer_lines.max(1) as u64;
    let fail_n = args.drain_fail;
    writer.set_fault(Fault {
        step,
        remaining: fail_n,
    });
    let wbase = writer.watermark();
    let mut queue: VecDeque<PendingLine> = VecDeque::new();
    let mut queued_frames = 0usize;
    let mut queued_bytes = 0usize;
    let mut peak_frames = 0usize;
    let mut peak_bytes = 0usize;
    let mut dropped: Vec<u64> = Vec::new();
    let mut produced: u64 = 0;
    let mut flushed: u64 = 0;
    let mut failed: u32 = 0;
    let mut attempts: u32 = 0;
    let attempt_bound = fail_n.saturating_mul(2) + u32::try_from(k.saturating_mul(4)).unwrap() + 64;

    while produced < k || !queue.is_empty() {
        if produced < k {
            produced += 1;
            let content = line_content(wbase + produced, args.line_bytes);
            let event_payload = format!(r#"{{"producer_index":{produced}}}"#);
            let frames = split_line(&content).len();
            let bytes = content.len();
            if queued_frames + frames > DRAIN_QUEUE_FRAMES
                || queued_bytes + bytes > DRAIN_QUEUE_BYTES
            {
                // Overflow: the bounded queue refuses the line; its number is
                // dropped with an explicit missing range (never written,
                // never reused).
                dropped.push(produced);
            } else {
                queued_frames += frames;
                queued_bytes += bytes;
                peak_frames = peak_frames.max(queued_frames);
                peak_bytes = peak_bytes.max(queued_bytes);
                queue.push_back(PendingLine {
                    index: produced,
                    content,
                    event_payload,
                });
            }
        }
        if let Some(front) = queue.front() {
            attempts += 1;
            if attempts > attempt_bound {
                bail!("drain flush attempts exceeded the safety bound");
            }
            let line = wbase + front.index;
            let event = ("output", front.event_payload.clone());
            let content = front.content.clone();
            match writer
                .append_line_numbered(line, &content, Some(event), None)
                .await
            {
                Ok(p) => {
                    let flushed_line = queue.pop_front().unwrap();
                    queued_frames -= split_line(&flushed_line.content).len();
                    queued_bytes -= flushed_line.content.len();
                    flushed += 1;
                    emit_json(&published_record(&p));
                }
                Err(_) => {
                    // Injected failure (or its abandoned-segment debris);
                    // the line stays queued and is retried.
                    failed += 1;
                }
            }
        }
    }
    if failed != fail_n {
        bail!("drain injected {failed} failures, expected {fail_n}");
    }

    let mut gap: Option<(u64, u64)> = None;
    let terminal = args
        .terminal
        .clone()
        .unwrap_or_else(|| TERMINAL_ID.to_string());
    if !dropped.is_empty() {
        let first = wbase + dropped[0];
        let last = wbase + dropped[dropped.len() - 1] + 1;
        // Explicit missing range: never fabricate continuity, never reuse the
        // dropped numbers. Also latch degraded + refuse-new-start.
        store.add_log_gap(&terminal, first, last, "missing").await?;
        store.set_degraded(&terminal).await?;
        store.set_refuse_new_start(&terminal).await?;
        gap = Some((first, last));
    }
    emit_json(&serde_json::json!({ "drain": {
        "step": args.drain_step,
        "produced": produced,
        "flushed": flushed,
        "dropped": dropped.len(),
        "dropped_first": dropped.first().copied(),
        "dropped_last": dropped.last().copied(),
        "queue_frames": DRAIN_QUEUE_FRAMES,
        "queue_bytes": DRAIN_QUEUE_BYTES,
        "peak_queued_frames": peak_frames,
        "peak_queued_bytes": peak_bytes,
        "injected_failures": failed,
        "gap": gap,
        "watermark": writer.watermark(),
        "refuse_new_start": gap.is_some(),
    }}));
    Ok(())
}

async fn run_writer(args: &WriterArgs) -> Result<()> {
    let workdir = args.workdir.as_ref().context("--workdir is required")?;
    let migrations = args
        .migrations
        .as_ref()
        .context("--migrations is required")?;
    std::fs::create_dir_all(workdir)?;
    if let Some(p) = &args.crash {
        if !CRASH_POINTS.contains(&p.as_str()) {
            bail!("unknown crash point {p}");
        }
    }
    let ctl = crash::CrashCtl::new(args.crash.as_deref(), args.trace.as_deref())?;
    ctl.step("writer_start");
    let store_ctl = crash::CrashCtl::new(args.crash.as_deref(), args.trace.as_deref())?;
    let store =
        Arc::new(Store::open_with_crash(&workdir.join(DB_FILE), migrations, store_ctl).await?);
    let mut writer = Writer::open_with(
        store.clone(),
        workdir,
        args.terminal.as_deref().unwrap_or(TERMINAL_ID),
        if args.max_segment_bytes == 0 {
            MAX_SEGMENT_BYTES
        } else {
            args.max_segment_bytes
        },
    )
    .await?;
    if let Some(gate) = &args.pre_commit_gate {
        writer.set_pre_commit_gate(gate.clone());
    }

    if args.drain_step.is_some() {
        run_drain(&mut writer, &store, args).await?;
    } else {
        for _ in 0..args.prelude {
            let line = writer.watermark() + 1;
            let content = line_content(line, args.line_bytes.max(32));
            append_one(&mut writer, &ctl, args, &content, None).await?;
        }
        if args.long_utf8 > 0 {
            let content = "界".repeat(args.long_utf8);
            let exit = None;
            append_one(&mut writer, &ctl, args, &content, exit).await?;
        }
        for i in 0..args.append {
            let line = writer.watermark() + 1;
            let content = line_content(line, args.line_bytes.max(32));
            let exit = if i + 1 == args.append {
                args.with_exit.map(|c| (c, "clean"))
            } else {
                None
            };
            append_one(&mut writer, &ctl, args, &content, exit).await?;
        }
    }
    emit_json(&serde_json::json!({ "writer_done": {
        "watermark": writer.watermark(),
        "epoch": Uuid::from_bytes(writer.epoch()).to_string(),
    }}));
    Ok(())
}

async fn run_recover(
    workdir: &Path,
    migrations: &Path,
    terminal: &str,
    crash: Option<&str>,
    trace: Option<&Path>,
) -> Result<()> {
    if let Some(p) = crash {
        if !RECOVERY_CRASH_POINTS.contains(&p) {
            bail!("unknown recovery crash point {p}");
        }
    }
    let ctl = crash::CrashCtl::new(crash, trace)?;
    let store = Store::open_with_crash(&workdir.join(DB_FILE), migrations, ctl).await?;
    let report = recovery::recover(&store, workdir, terminal).await?;
    println!("RECOVER {}", serde_json::to_string(&report)?);
    store.close().await;
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: qingluan-terminal-storage-probe <command> [args]\n\
         commands:\n\
           writer    --workdir DIR --migrations DIR [--terminal ID]\n\
                     [--max-segment-bytes N] [--crash POINT] [--trace FILE]\n\
                     [--prelude N] [--append N] [--line-bytes N] [--long-utf8 N]\n\
                     [--event-per-line] [--with-exit CODE]\n\
                     [--drain-step append|sync|commit --drain-fail N --producer-lines N]\n\
                     [--gate-after-line N --gate-file FILE]\n\
                     [--pre-commit-gate FILE]\n\
           recover   --workdir DIR --migrations DIR [--terminal ID]\n\
                     [--crash rec_artifact_written|rec_tail_truncated|rec_file_quarantined]\n\
                     [--trace FILE]\n\
           selfcheck [--migrations DIR] [--workdir DIR]\n\
           gate-c    --workdir DIR --migrations DIR"
    );
}

fn parse_common_args(
    args: &[String],
    mut on_command: impl FnMut(&str),
) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<String>)> {
    let mut workdir = None;
    let mut migrations = None;
    let mut terminal = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--workdir" => {
                i += 1;
                workdir = Some(PathBuf::from(
                    args.get(i).context("--workdir needs a value")?,
                ));
            }
            "--migrations" => {
                i += 1;
                migrations = Some(PathBuf::from(
                    args.get(i).context("--migrations needs a value")?,
                ));
            }
            "--terminal" => {
                i += 1;
                terminal = Some(args.get(i).context("--terminal needs a value")?.clone());
            }
            cmd if cmd.starts_with('-') => bail!("handled by the command parser: {cmd}"),
            other => on_command(other),
        }
        i += 1;
    }
    Ok((workdir, migrations, terminal))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().cloned() else {
        print_usage();
        bail!("expected a command");
    };
    let rest = &args[1..];
    match command.as_str() {
        "writer" => {
            let mut wa = WriterArgs::default();
            let mut i = 0;
            while i < rest.len() {
                let a = &rest[i];
                match a.as_str() {
                    "--workdir" => {
                        i += 1;
                        wa.workdir = Some(PathBuf::from(rest.get(i).context("value")?));
                    }
                    "--migrations" => {
                        i += 1;
                        wa.migrations = Some(PathBuf::from(rest.get(i).context("value")?));
                    }
                    "--terminal" => {
                        i += 1;
                        wa.terminal = Some(rest.get(i).context("value")?.clone());
                    }
                    "--max-segment-bytes" => {
                        i += 1;
                        wa.max_segment_bytes = rest.get(i).context("value")?.parse()?;
                    }
                    "--crash" => {
                        i += 1;
                        wa.crash = Some(rest.get(i).context("value")?.clone());
                    }
                    "--trace" => {
                        i += 1;
                        wa.trace = Some(PathBuf::from(rest.get(i).context("value")?));
                    }
                    "--prelude" => {
                        i += 1;
                        wa.prelude = rest.get(i).context("value")?.parse()?;
                    }
                    "--append" => {
                        i += 1;
                        wa.append = rest.get(i).context("value")?.parse()?;
                    }
                    "--line-bytes" => {
                        i += 1;
                        wa.line_bytes = rest.get(i).context("value")?.parse()?;
                    }
                    "--long-utf8" => {
                        i += 1;
                        wa.long_utf8 = rest.get(i).context("value")?.parse()?;
                    }
                    "--event-per-line" => wa.event_per_line = true,
                    "--with-exit" => {
                        i += 1;
                        wa.with_exit = Some(rest.get(i).context("value")?.parse()?);
                    }
                    "--drain-step" => {
                        i += 1;
                        wa.drain_step = Some(rest.get(i).context("value")?.clone());
                    }
                    "--drain-fail" => {
                        i += 1;
                        wa.drain_fail = rest.get(i).context("value")?.parse()?;
                    }
                    "--producer-lines" => {
                        i += 1;
                        wa.producer_lines = rest.get(i).context("value")?.parse()?;
                    }
                    "--gate-after-line" => {
                        i += 1;
                        wa.gate_after_line = Some(rest.get(i).context("value")?.parse()?);
                    }
                    "--gate-file" => {
                        i += 1;
                        wa.gate_file = Some(PathBuf::from(rest.get(i).context("value")?));
                    }
                    "--pre-commit-gate" => {
                        i += 1;
                        wa.pre_commit_gate = Some(PathBuf::from(rest.get(i).context("value")?));
                    }
                    other => {
                        print_usage();
                        bail!("unknown writer argument: {other}");
                    }
                }
                i += 1;
            }
            if wa.line_bytes == 0 {
                wa.line_bytes = 120;
            }
            if let Err(e) = run_writer(&wa).await {
                let msg = format!("{e:#}");
                if msg.starts_with("REFUSE-NEW-START") {
                    emit_json(&serde_json::json!({ "refuse_new_start": true, "error": msg }));
                    std::process::exit(REFUSE_EXIT);
                }
                eprintln!("ERROR {msg}");
                std::process::exit(1);
            }
            Ok(())
        }
        "recover" => {
            // Extract --crash/--trace before the common parser (which
            // rejects unknown flags); the recovery crash checkpoints fire
            // inside the quarantine mutations.
            let mut crash: Option<String> = None;
            let mut trace: Option<PathBuf> = None;
            let mut common: Vec<String> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--crash" => {
                        i += 1;
                        crash = Some(rest.get(i).context("--crash needs a value")?.clone());
                    }
                    "--trace" => {
                        i += 1;
                        trace = Some(PathBuf::from(rest.get(i).context("--trace needs a value")?));
                    }
                    other => common.push(other.to_string()),
                }
                i += 1;
            }
            let (workdir, migrations, terminal) = parse_common_args(&common, |_| {})?;
            let workdir = workdir.context("--workdir DIR is required")?;
            let migrations = migrations.context("--migrations DIR is required")?;
            let terminal = terminal.unwrap_or_else(|| TERMINAL_ID.to_string());
            run_recover(
                &workdir,
                &migrations,
                &terminal,
                crash.as_deref(),
                trace.as_deref(),
            )
            .await
        }
        "gate-c" => {
            let (workdir, migrations, _terminal) = parse_common_args(rest, |_| {})?;
            let workdir = workdir.context("--workdir DIR is required")?;
            let migrations = migrations.context("--migrations DIR is required")?;
            let summary = gate::gate_c(&workdir, &migrations).await?;
            std::fs::write(
                workdir.join("summary.json"),
                serde_json::to_string_pretty(&summary)?,
            )?;
            println!(
                "GATE-C-OK points={} scenarios={} power_loss_variants={} crashed_children={} recover_children={}",
                summary.points.iter().filter(|p| p.ok).count(),
                summary.scenarios.iter().filter(|s| s.ok).count(),
                summary.points.iter().filter(|p| p.power_loss).count(),
                summary.crashed_children,
                summary.recover_children
            );
            Ok(())
        }
        "selfcheck" => {
            let (workdir, migrations, _terminal) = parse_common_args(rest, |_| {})?;
            let migrations =
                migrations.context("--migrations DIR is required (runtime Migrator::new)")?;
            let workdir = workdir.unwrap_or_else(|| {
                std::env::temp_dir()
                    .join(format!("ql-storage-selfcheck-{}", Uuid::now_v7().simple()))
            });
            std::fs::create_dir_all(&workdir)?;
            let summary = harness::selfcheck(&workdir, &migrations).await?;
            let path = workdir.join("summary.json");
            std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
            println!("SELF-CHECK-OK workdir={}", workdir.display());
            Ok(())
        }
        other => {
            print_usage();
            bail!("unknown command: {other}");
        }
    }
}
