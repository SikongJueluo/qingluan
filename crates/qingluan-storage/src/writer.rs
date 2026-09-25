//! The terminal log writer: durability-ordered appends on two streams,
//! behind the persistence batcher.
//!
//! Each log carries two independent streams (one shared `log_epoch`):
//! the normalized UTF-8 line stream (`append_line`, 1-based line numbers,
//! never reused) and the raw byte stream (`append_raw`, arbitrary bytes
//! addressed by stream byte offset). Each stream owns its active segment,
//! its `frame_seq` sequence, and its watermark; segments are kind-tagged
//! so a frame of one stream can never be stored under (or read back
//! through) the other.
//!
//! Persistence batching (storage-facing and PTY-independent): each stream
//! buffers its accepted appends in a bounded pending batch — bounded by
//! [`FLUSH_MAX_BYTES`] (64 KiB, flushed synchronously by the append that
//! reaches it) and by [`FLUSH_MAX_DELAY`] (50 ms from the batch's first
//! accepted byte, enforced by an internal flush driver task) — or by an
//! explicit [`LogWriter::flush`]. Sealing a stream flushes it first, so
//! buffered data always lands in the segment being sealed. Dropping the
//! writer does not flush: bytes that were only buffered are not durable
//! (callers that need durability call [`LogWriter::flush`]).
//!
//! Commit sequence per flushed batch (the production invariant):
//!
//! 1. append complete frames for the whole batch (lines may span frames;
//!    raw appends may span frames);
//! 2. `sync_data` the file (the segment header was synced at creation, and
//!    the parent directory fsynced, before any frame);
//! 3. one short SQLite transaction publishes the committed state of that
//!    stream (no file I/O, no client wait inside the transaction);
//! 4. only then is the in-memory watermark published.
//!
//! Bytes written and synced but not yet committed by step 3 are invisible
//! to every read (committed-prefix visibility). All file I/O runs on the
//! async runtime's blocking pool (`tokio::fs`); no async method performs
//! blocking filesystem calls on the executor thread.
//!
//! Cancellation safety of the foreground operations (`append_line`,
//! `append_raw`, `flush`, `seal`, `close`): each runs its acceptance and
//! persistence inside a spawned command task that owns a clone of the
//! writer's shared state — and through it the exclusive writer lease —
//! until the batch's Tokio fs and SQLite work has truly completed.
//! Awaiting that task is the caller's choice; dropping or aborting the
//! awaiting future only detaches it, so no cancellation can strand an
//! extracted batch, leave stale cached segment state behind, or release
//! the lease while this writer's file/commit work is still in flight.
//! Every command registers itself in an in-flight count *before* it is
//! spawned, and `close` owns its entire shutdown sequence in one
//! spawned task created before the method's first await: signal close,
//! join the flush driver, quiesce every detached foreground command,
//! then drain both streams — so even a caller that aborts `close`
//! mid-shutdown only detaches that sequence, and the shared state (with
//! the lease) stays held until the final drain is truly complete. An
//! accepted line or byte range whose flush was already started
//! therefore always commits or latches exactly as an uncancelled caller
//! would have observed.
//!
//! Failure latching: the stream is latched recovery-required the moment a
//! flushed batch starts appending bytes, and the latch clears only after
//! that batch's visibility transaction committed and published. Any
//! write, sync, or commit failure — and any seal failure (file sync or
//! row seal, rotation or explicit, with the active segment kept in
//! place: its row is still the stream's active segment and a retry
//! without recovery must not create a second one behind it) — therefore
//! leaves the writer refusing every further append (and flush/seal) of
//! that stream with a typed [`StorageError::RecoveryRequired`] until
//! the recovery pass truncated the uncommitted tail — a retry can never
//! append behind stale cached state. Numbers of an accepted-but-unflushed batch are never committed;
//! after recovery they may be re-assigned (uncommitted numbers are not
//! events) — a retry without recovery refuses, so an accepted range can
//! never be silently skipped — while a batch dropped by an unsafe
//! retention reclaim *is* committed as an explicit stream-scoped gap with
//! its watermark advanced, and that drop is reported to the owner as a
//! durable per-stream outcome ([`LogWriter::flush`],
//! [`LogWriter::close`], [`LogWriter::last_flush_outcome`]) instead of
//! being swallowed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use qingluan_core::terminal::{LogEpoch, LogIdentity};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, watch};
use tokio::time::Instant;

use crate::LogStream;
use crate::crash::CrashPoint;
#[cfg(any(test, feature = "test-hooks"))]
use crate::crash::{ParkFuture, ParkPoint};
use crate::db::{CommitInput, SegmentCreation, SegmentRow, Store, StreamCommit, TerminalRow};
use crate::error::{StorageError, io_error};
use crate::frame::{
    FRAME_FLAG_LINE_END, FRAME_KIND_LINE, FRAME_KIND_RAW, FrameHeader, SEGMENT_HEADER_LEN,
    ScanOutcome, SegmentHeader, encode_frame, scan_frames, split_line, split_raw,
};
use crate::gap::{GapReason, GapSpan};
use crate::identity::{HeaderIdentity, LogKey, ResolvedIdentity};
use crate::lease::WriterLease;
use crate::paths;
use crate::recovery::frames_match_row;

/// Rotation threshold: a segment is sealed before an append that would
/// push it past 4 MiB (the plan's production value; the probe's 256 KiB
/// was probe-only). Applies per stream: each stream rotates its own
/// segments.
pub(crate) const MAX_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;

/// Persistence-policy flush delay: a pending batch is flushed at most
/// this long after its first accepted byte, whichever of this bound and
/// [`FLUSH_MAX_BYTES`] comes first. Enforced by the writer's internal
/// flush driver, not by the caller.
pub const FLUSH_MAX_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Persistence-policy flush bound: a pending batch holding this many
/// bytes is flushed by the append that reached the bound. The pending
/// buffer therefore never exceeds one caller batch beyond this bound.
pub const FLUSH_MAX_BYTES: usize = 64 * 1024;

/// Durability state of one accepted append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// Accepted into the pending batch; it becomes durable at the next
    /// flush boundary (size bound, deadline, seal, or explicit flush).
    /// An append that only buffered is not yet durable.
    Buffered,
    /// Durable and visible when the call returned (this append reached
    /// the size bound and its flush completed).
    Committed,
    /// Not persisted: the batch could not be given a segment (an unsafe
    /// retention reclaim). Its exact range was recorded as a
    /// stream-scoped gap, the watermark advanced past it (the numbers are
    /// consumed, never reused), and the terminal latched `degraded` +
    /// `refuse_new_start`. Draining continues.
    Dropped,
}

/// One accepted normalized line append and its durability state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendedLine {
    /// The line number just accepted (its text is durable only once the
    /// outcome is [`AppendOutcome::Committed`]).
    pub line: u64,
    /// What happened to the batch this line joined.
    pub outcome: AppendOutcome,
}

/// One accepted raw append and its durability state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendedRaw {
    /// Raw stream byte offset of the first byte of this append.
    pub offset: u64,
    /// Number of bytes accepted by this append.
    pub len: u64,
    /// What happened to the batch this append joined.
    pub outcome: AppendOutcome,
}

/// One explicitly recorded raw-stream loss.
///
/// Returned by [`LogWriter::record_raw_loss`]: the raw byte offset the
/// dropped range started at and its length. The range is consumed (its
/// offset can never be reused) and recorded as an explicit stream-scoped
/// gap, so a bounded reader can account for skipped bytes by coordinate
/// without inventing continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendedLoss {
    /// Raw stream byte offset of the first dropped byte.
    pub offset: u64,
    /// Number of bytes dropped by this loss.
    pub len: u64,
}

/// One explicitly recorded normalized-stream loss of line numbers.
///
/// Returned by [`LogWriter::record_line_loss`]: the first retired line
/// number (the line the writer would have used next) and how many were
/// retired. The range is consumed (its numbers can never be reused) and
/// recorded as an explicit stream-scoped gap, so a reader accounts for the
/// missing lines instead of inventing continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendedLineLoss {
    /// First retired line number.
    pub first_line: u64,
    /// Number of retired line numbers.
    pub lines: u64,
}

/// The outcome of flushing one stream's pending batch. This is the
/// durable per-stream outcome owners observe — on [`LogWriter::flush`],
/// on [`LogWriter::close`], and (for the internal flush driver's
/// deadline-initiated batches) on [`LogWriter::last_flush_outcome`] —
/// distinguishing a committed batch from one dropped as an explicit gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamFlushOutcome {
    /// Nothing was pending; no batch existed.
    Nothing = 0,
    /// The batch is durable and visible.
    Committed = 1,
    /// The batch was dropped as an explicit stream-scoped gap (an unsafe
    /// retention reclaim): its exact range is recorded, its numbers are
    /// consumed (never reused), and the terminal latched `degraded` +
    /// `refuse_new_start`. Draining continues.
    Dropped = 2,
}

impl StreamFlushOutcome {
    fn from_raw(value: u8) -> Self {
        match value {
            1 => StreamFlushOutcome::Committed,
            2 => StreamFlushOutcome::Dropped,
            _ => StreamFlushOutcome::Nothing,
        }
    }
}

/// The per-stream outcomes of one flush of both streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushOutcomes {
    /// The normalized stream's batch outcome.
    pub normalized: StreamFlushOutcome,
    /// The raw stream's batch outcome.
    pub raw: StreamFlushOutcome,
}

/// Fault-injection site for the flushed-commit sequence (test builds
/// only): the next batch reaching the site fails exactly there, which is
/// how the post-failure latch and its recovery convergence are proven.
/// The pre-frame sites fail the operations that run after a batch was
/// extracted but before its first frame byte is written (a rotation
/// seal, and the fresh segment's creation path); the seal sites also
/// cover the explicit [`LogWriter::seal`] steps.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultSite {
    /// Fail a seal's file `sync_data` — rotation or explicit: the active
    /// segment still holds the writer's committed state and nothing new
    /// was written yet.
    SealSync,
    /// Fail the seal's segment-row update (the file sync succeeded).
    SealDb,
    /// Fail the unlink of a segment file reclaimed by the creation
    /// transaction (the row deletion already committed).
    ReclaimUnlink,
    /// Fail the fresh segment header's `write_all` (the empty file was
    /// created exclusively).
    HeaderWrite,
    /// Fail the fresh segment header's `sync_data` (the header bytes are
    /// written but not durable).
    HeaderSync,
    /// Fail the creation-path parent-directory `fsync` (after the
    /// reclaimed-file unlink, or after the header sync).
    DirSync,
    /// Write only the first half of the frame buffer, then fail.
    PartialFrameWrite,
    /// Fail the file `sync_data` after a complete write.
    FrameSync,
    /// Fail the visibility transaction after a complete sync.
    Commit,
}

/// Outcome of the writer-side segment-creation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateOutcome {
    /// A fresh active segment exists with a synced header.
    Created,
    /// The creation was refused (no safely reclaimable sealed segment);
    /// the caller drops its batch instead.
    UnsafeReclaim,
}

struct ActiveSegment {
    row: SegmentRow,
    file: tokio::fs::File,
    len: u64,
}

/// Writer-side state of one stream: its active segment, the next frame
/// sequence number to allocate inside it, and the failure latch.
struct StreamState {
    active: Option<ActiveSegment>,
    next_frame_seq: u64,
    /// Latched from the moment a flushed batch starts appending bytes
    /// until that batch's visibility transaction committed and published.
    /// While set, every append/flush/seal of this stream refuses with a
    /// typed [`StorageError::RecoveryRequired`] — the cached
    /// `len`/`next_frame_seq` may be stale behind an uncommitted tail and
    /// only recovery may truncate it.
    recovery_required: Option<String>,
}

/// Everything the flush paths mutate, guarded by one lock so appends, the
/// deadline driver, seals, and flushes serialize per writer.
struct WriterState {
    normalized: StreamState,
    raw: StreamState,
    pending_lines: Vec<(u64, String)>,
    pending_raw: Vec<u8>,
    /// Raw stream offset the pending raw buffer continues from.
    pending_raw_start: u64,
    norm_deadline: Option<Instant>,
    raw_deadline: Option<Instant>,
    max_segment_bytes: u64,
    #[cfg(any(test, feature = "test-hooks"))]
    park: Option<(ParkPoint, ParkFuture)>,
    #[cfg(any(test, feature = "test-hooks"))]
    fault: Option<FaultSite>,
}

/// State shared by the writer handle and its flush-driver task.
struct WriterShared {
    store: Arc<Store>,
    root: PathBuf,
    key: LogKey,
    identity: HeaderIdentity,
    epoch: LogEpoch,
    state: Mutex<WriterState>,
    /// Fired whenever a pending batch opens (a deadline may have moved)
    /// or the handle signals shutdown.
    opened: Notify,
    /// The log's exclusive writer lease. It lives here — not on the
    /// handle — so it is held until the flush driver *actually* exits:
    /// dropping the handle signals `closing` and detaches the driver
    /// (never aborts it, so an in-flight blocking-pool file operation
    /// always completes before the lease can release), and recovery or a
    /// new writer therefore cannot interleave with in-flight driver I/O.
    /// The foreground command tasks (append/flush/seal/close bodies) hold
    /// the same clone and the same guarantee: a cancelled caller detaches
    /// its command, which keeps the lease until its fs/SQLite work truly
    /// completes. [`LogWriter::close`] quiesces the driver and every
    /// command before returning, so on its return the lease is
    /// deterministically released. The field is held for its drop (the
    /// descriptor's lifetime is the lease), never read.
    #[allow(dead_code)]
    lease: WriterLease,
    /// Shutdown signal for the flush driver: set by `close` and by
    /// `Drop`; the driver exits at the top of its loop once set.
    closing: AtomicBool,
    /// In-flight foreground command tasks (append/flush/seal), published
    /// as a count through a watch channel so the shutdown sequence can
    /// await a quiet writer with no lost-wakeup race: a command
    /// increments the count *before* it is spawned (no await separates
    /// the two) and its [`CommandGuard`] decrements it when the command
    /// body finishes — even when the spawning caller was cancelled first
    /// — so a `close` that starts while a detached command is still
    /// running always waits for that command before its final drain, and
    /// the lease (which every command holds through this shared state)
    /// releases only after it. The shutdown task itself is not counted:
    /// nothing may wait for it.
    commands: watch::Sender<usize>,
    /// Logical watermarks (the last *accepted* line/offset, buffered
    /// included). Mutated only while holding the state lock; readable
    /// without it. After a failed flush they stay at the accepted value
    /// while the durable watermark sits lower — the writer refuses
    /// further appends until recovery realigns them.
    line_watermark: AtomicU64,
    raw_watermark: AtomicU64,
    /// The durable outcome of the most recent batch-bearing flush of
    /// each stream (explicit, size-bound, or driver-initiated): how the
    /// owner observes a driver-initiated drop while the producer keeps
    /// draining. `Nothing` flushes never overwrite a real outcome.
    norm_outcome: AtomicU8,
    raw_outcome: AtomicU8,
}

impl WriterShared {
    fn record_outcome(&self, stream: LogStream, outcome: StreamFlushOutcome) {
        let slot = match stream {
            LogStream::Normalized => &self.norm_outcome,
            LogStream::Raw => &self.raw_outcome,
        };
        slot.store(outcome as u8, Ordering::Release);
    }

    fn last_outcome(&self, stream: LogStream) -> StreamFlushOutcome {
        let value = match stream {
            LogStream::Normalized => self.norm_outcome.load(Ordering::Acquire),
            LogStream::Raw => self.raw_outcome.load(Ordering::Acquire),
        };
        StreamFlushOutcome::from_raw(value)
    }

    /// Register one foreground command before it is spawned. Synchronous
    /// on purpose: no await separates the increment from the spawn, so a
    /// `close` starting concurrently can never miss the command, and the
    /// returned guard — owned from then on by the spawned task — drops
    /// the registration exactly when the command body finishes.
    fn enter_command(self: &Arc<Self>) -> CommandGuard {
        self.commands.send_modify(|count| *count += 1);
        CommandGuard {
            sender: self.commands.clone(),
        }
    }

    /// Wait until every foreground command task has finished. Used by
    /// the shutdown sequence between joining the flush driver and the
    /// final drain, so a detached command's in-flight fs/SQLite work
    /// lands before the drain reads the pending state and never outlives
    /// the lease's release.
    async fn wait_for_commands_to_quiesce(&self) {
        let mut quiet = self.commands.subscribe();
        // The sender lives in the shared state, so the channel never
        // closes while a shutdown is running; only the predicate matters.
        let _ = quiet.wait_for(|count| *count == 0).await;
    }
}

/// Ends one foreground command's in-flight registration (see
/// [`WriterShared::commands`]); dropped by the command task when its
/// body finishes, whichever way it ends.
struct CommandGuard {
    sender: watch::Sender<usize>,
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        self.sender
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// Writer for one terminal log. Created via
/// [`crate::LogStore::open_writer`]; not `Clone` (it owns the internal
/// flush-driver task, which outlives a dropped handle until it exits).
pub struct LogWriter {
    shared: Arc<WriterShared>,
    driver: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if let Some(driver) = self.driver.take() {
            // Signal closing; never abort. Cancellation can land while a
            // `tokio::fs` operation is awaiting its blocking-pool job:
            // aborting would drop the driver future — and with it the
            // `WriterShared`/`WriterLease` reference — even though the
            // underlying file operation is not cancellable and keeps
            // running, so recovery or a reopen could take the lease while
            // this writer's segment I/O is still in flight. Detaching lets
            // the driver finish any in-flight operation and release the
            // lease exactly when it exits (a reaper task joins it whenever
            // a runtime context exists). The foreground command tasks have
            // the same shape by construction: they are spawned, never
            // aborted, so a cancelled `append_line`/`append_raw`/`flush`/
            // `seal`/`close` caller detaches its command, which keeps the
            // lease until its persistence work truly completes. Buffered-
            // but-unflushed batches stay non-durable: the driver exits at
            // its next loop check without opening a new flush.
            self.shared.closing.store(true, Ordering::Release);
            self.shared.opened.notify_one();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = driver.await;
                });
            }
        }
    }
}

impl LogWriter {
    pub(crate) async fn attach(
        store: Arc<Store>,
        root: &std::path::Path,
        log: &LogIdentity,
    ) -> Result<LogWriter, StorageError> {
        // Identity validation happens before any file or database mutation.
        let ident = ResolvedIdentity::parse(log)?;
        // Exactly one writer per log, across every store handle and every
        // process sharing this root: two writers attaching the same
        // watermark would commit overlapping ranges. The OS lease is
        // kernel-released on process death, so a crashed writer never
        // leaves a stale one. Acquired *before* the terminal row is read:
        // the row read, the latch/identity checks, the watermark
        // initialization, and the active-segment attach below all run
        // while this writer alone holds the lease, so a waiter that
        // queued behind a closing owner attaches at that owner's final
        // durable state instead of the stale snapshot it could have read
        // before waiting.
        let lease = WriterLease::acquire(root, &ident.key).await?;
        let (term, _created) = store.ensure_terminal(&ident.key, ident.header).await?;
        if term.refuse_new_start {
            return Err(StorageError::RefuseNewStart {
                detail: "terminal latched a drain overflow or an unsafe retention reclaim; \
                         a destructive rebuild is required"
                    .into(),
            });
        }
        // `degraded` is latched by recovery when explicit gaps exist (or by
        // an unsafe reclaim's dropped batch): writing continues at
        // watermark + 1 in fresh segments — the loss is observable state,
        // not a write blocker. `refuse_new_start` above is the write
        // blocker, and it only rejects *new* starts: the already-running
        // writer keeps draining (recording explicit gaps for what it must
        // drop).
        if term.log_epoch != ident.header.epoch {
            return Err(StorageError::EpochMismatch {
                stored: uuid::Uuid::from_bytes(term.log_epoch).to_string(),
                requested: ident.epoch,
            });
        }
        // The row's on-disk identity must match the parsed one exactly, so a
        // tampered or restored database can never be adopted silently
        // (canonical UUID parsing already keeps text keys 1:1 with the
        // 16-byte identity).
        if term.terminal_uuid != ident.header.terminal_uuid {
            return Err(StorageError::TerminalUuidMismatch {
                stored: uuid::Uuid::from_bytes(term.terminal_uuid).to_string(),
                requested: ident.terminal_id,
            });
        }
        let shared = Arc::new(WriterShared {
            store,
            root: root.to_path_buf(),
            key: ident.key,
            identity: ident.header,
            epoch: log.log_epoch.clone(),
            state: Mutex::new(WriterState {
                normalized: StreamState {
                    active: None,
                    next_frame_seq: 0,
                    recovery_required: None,
                },
                raw: StreamState {
                    active: None,
                    next_frame_seq: 0,
                    recovery_required: None,
                },
                pending_lines: Vec::new(),
                pending_raw: Vec::new(),
                pending_raw_start: 0,
                norm_deadline: None,
                raw_deadline: None,
                max_segment_bytes: MAX_SEGMENT_BYTES,
                #[cfg(any(test, feature = "test-hooks"))]
                park: None,
                #[cfg(any(test, feature = "test-hooks"))]
                fault: None,
            }),
            opened: Notify::new(),
            lease,
            closing: AtomicBool::new(false),
            commands: watch::Sender::new(0),
            line_watermark: AtomicU64::new(term.line_watermark),
            raw_watermark: AtomicU64::new(term.raw_watermark),
            norm_outcome: AtomicU8::new(StreamFlushOutcome::Nothing as u8),
            raw_outcome: AtomicU8::new(StreamFlushOutcome::Nothing as u8),
        });
        attach_active_segment(&shared, &term, LogStream::Normalized).await?;
        attach_active_segment(&shared, &term, LogStream::Raw).await?;
        let driver = tokio::spawn(drive(Arc::clone(&shared)));
        Ok(LogWriter {
            shared,
            driver: Some(driver),
        })
    }

    /// Last accepted normalized line number (buffered batches included);
    /// the next `append_line` must be `line_watermark + 1`. After a
    /// failure the durable watermark may sit lower until recovery ran.
    pub fn line_watermark(&self) -> u64 {
        self.shared.line_watermark.load(Ordering::Relaxed)
    }

    /// Number of raw bytes accepted by the raw stream (buffered batches
    /// included); the next `append_raw` continues at exactly this offset.
    pub fn raw_watermark(&self) -> u64 {
        self.shared.raw_watermark.load(Ordering::Relaxed)
    }

    /// Epoch this writer was opened against (shared by both streams).
    pub fn epoch(&self) -> &LogEpoch {
        &self.shared.epoch
    }

    /// Append one normalized UTF-8 line. `line` must be exactly
    /// `line_watermark + 1` (line numbers are never reused; dropped
    /// ranges are explicit gaps). The line joins this stream's pending
    /// batch and becomes durable at the next flush boundary: the 64 KiB
    /// batch bound (this call then flushes synchronously and returns
    /// [`AppendOutcome::Committed`]), the 50 ms deadline, a seal, or an
    /// explicit [`LogWriter::flush`].
    ///
    /// Cancellation-safe by construction: the acceptance and any
    /// size-bound flush run inside a spawned command task (see the module
    /// docs), so a caller that drops this future mid-append cannot
    /// strand the accepted line or release the lease mid-operation — the
    /// command completes the append and surfaces its outcome through the
    /// writer's durable state either way.
    pub async fn append_line(
        &mut self,
        line: u64,
        text: &str,
    ) -> Result<AppendedLine, StorageError> {
        let shared = Arc::clone(&self.shared);
        let text = text.to_owned();
        // Registered before the spawn so a concurrently starting `close`
        // always waits for this command before draining.
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            append_line_command(shared, line, text).await
        }))
        .await
    }

    /// Append arbitrary raw bytes. Raw payloads have no character
    /// boundary to respect (NUL and invalid UTF-8 round-trip exactly);
    /// the append continues the raw stream at exactly `raw_watermark` in
    /// this stream's pending batch, flushed under the same 64 KiB / 50 ms
    /// policy as the normalized stream (independently: each stream owns
    /// its batch, segments, and transactions). An empty append is
    /// accepted with `len: 0` at the current offset but buffers no
    /// bytes and commits no frame: the next flush boundary finds nothing
    /// pending for it. Cancellation-safe by construction, exactly like
    /// [`LogWriter::append_line`].
    pub async fn append_raw(&mut self, bytes: &[u8]) -> Result<AppendedRaw, StorageError> {
        let shared = Arc::clone(&self.shared);
        let bytes = bytes.to_vec();
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            append_raw_command(shared, bytes).await
        }))
        .await
    }

    /// Record an explicit raw-stream loss of `len` bytes for a bounded
    /// reader that must discard bytes it cannot retain.
    ///
    /// The bounded PTY reader drains output and hands it to
    /// [`LogWriter::append_raw`]; when its own buffer is full it discards
    /// a run instead of blocking the program. This records that run as an
    /// explicit raw gap and advances the non-reusable raw watermark past
    /// it, returning the exact coordinate ([`AppendedLoss`]) so the reader
    /// never accounts a skipped byte as present. Any pending raw batch is
    /// committed first and the current segment is sealed before the gap is
    /// recorded, so the loss falls on a segment boundary instead of
    /// punching a hole that re-reading would classify as corruption; the
    /// terminal latches `degraded` (+ `refuse_new_start`, matching the
    /// existing drop path), and draining continues. `len == 0` is a no-op
    /// at the current offset. Cancellation-safe by construction, like
    /// [`LogWriter::append_raw`].
    pub async fn record_raw_loss(&mut self, len: u64) -> Result<AppendedLoss, StorageError> {
        let shared = Arc::clone(&self.shared);
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            record_raw_loss_command(shared, len).await
        }))
        .await
    }

    /// Record an explicit normalized-stream loss of `lines` line numbers
    /// starting at `first_line`, which must be exactly the next line this
    /// writer would use (`line_watermark + 1`): a loss may consume numbers
    /// but never skip one.
    ///
    /// The bounded line normalizer uses this when one logical line grows
    /// past the bytes a mutable tail may hold: the bytes it must drop are a
    /// prefix of a line that does not exist yet, which the frame format
    /// cannot express (a line is always addressed from offset 0). Retiring
    /// the line number that content would have used records the loss as an
    /// explicit stream-scoped gap instead of persisting a truncated suffix
    /// as if it were a whole line. Any pending normalized batch is
    /// committed first and the active segment is sealed, so the discarded
    /// range is not covered by any segment's indexed range; the terminal
    /// latches `degraded` + `refuse_new_start`, and the numbers are
    /// consumed (never reused, never silently skipped). `lines == 0` is a
    /// no-op at the current watermark. Cancellation-safe by construction,
    /// like [`LogWriter::record_raw_loss`].
    ///
    /// The loss itself is *not* a batch: it is reported through its return
    /// value and the terminal's latches, and it deliberately leaves
    /// [`LogWriter::last_flush_outcome`] showing the last real **batch**
    /// outcome of the normalized stream. An owner that has to decide whether
    /// accepted lines are durable therefore keeps reading the batch that
    /// this operation committed on its way, instead of mistaking the
    /// retired range for a dropped batch.
    pub async fn record_line_loss(
        &mut self,
        first_line: u64,
        lines: u64,
    ) -> Result<AppendedLineLoss, StorageError> {
        let shared = Arc::clone(&self.shared);
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            record_line_loss_command(shared, first_line, lines).await
        }))
        .await
    }

    /// Force a batch boundary: flush both streams' pending batches (each
    /// through its own append → `sync_data` → visibility transaction) and
    /// return once they are durable, with each stream's outcome —
    /// committed, or dropped as an explicit stream-scoped gap (an unsafe
    /// retention reclaim; the numbers are consumed and never reused).
    /// A stream latched recovery-required refuses instead of appending
    /// behind its uncommitted tail. Cancellation-safe by construction:
    /// the flush runs inside a spawned command task that keeps the lease
    /// until both batches are truly durable (see the module docs).
    pub async fn flush(&mut self) -> Result<FlushOutcomes, StorageError> {
        let shared = Arc::clone(&self.shared);
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            flush_command(shared).await
        }))
        .await
    }

    /// The durable outcome of the most recent batch-bearing flush of one
    /// stream — explicit (`flush`), size-bound (the append that reached
    /// the 64 KiB bound), or driver-initiated (the 50 ms deadline). This
    /// is how an owner observes a *timer-driven* drop while the producer
    /// keeps draining: the driver records the outcome durably (explicit
    /// gap recorded, watermark advanced, `degraded` + `refuse_new_start`
    /// latched) and the writer keeps accepting appends.
    pub fn last_flush_outcome(&self, stream: LogStream) -> StreamFlushOutcome {
        self.shared.last_outcome(stream)
    }

    /// Graceful shutdown: signal the flush driver, join it (an in-flight
    /// deadline flush completes first — this never cancels I/O mid-flight),
    /// quiesce every detached foreground command task, then drain both
    /// streams' pending batches and return their outcomes. The whole
    /// sequence — signal, driver join, quiesce, drain — runs inside one
    /// spawned task created before `close`'s first await, so it is owned
    /// by the writer, not by the caller: when this returns, the driver
    /// and every command have exited and the exclusive writer lease is
    /// released, so recovery or a new writer may attach immediately —
    /// there is no window in which this writer's I/O still races a
    /// recovery or reopen.
    ///
    /// A cancelled `close` (its future dropped or aborted mid-shutdown)
    /// keeps the same guarantees: aborting the caller only detaches the
    /// owned shutdown task, which keeps the shared state — and through it
    /// the writer lease — until the driver has exited, every command has
    /// quiesced, and the pending batches are truly durable; a reopen or
    /// recovery waits for that teardown instead of interleaving with it
    /// (the bounded lease wait covers it). The final drain attempts both
    /// streams even when the first one fails — the isolation of
    /// [`LogWriter::flush`] — so a poisoned stream never strands the
    /// healthy one's trailing batch; the first error (normalized before
    /// raw) is reported deterministically.
    ///
    /// A stream whose final drain finds nothing pending reports the most
    /// recent batch-bearing outcome recorded for it — an explicit flush,
    /// the size-bound append, or the driver's deadline flush — instead of
    /// `Nothing`: if the 50 ms driver already committed (or durably
    /// dropped) the final batch, `close` still reports that outcome to the
    /// owner rather than a false "nothing was pending".
    ///
    /// [`Drop`] never aborts and never flushes: it signals the driver and
    /// detaches it, so buffered bytes are not durable (callers that need
    /// durability call this or [`LogWriter::flush`]) while an in-flight
    /// flush still completes. The lease releases only once that driver
    /// exits, which is why an immediate reopen may briefly wait for it.
    pub async fn close(mut self) -> Result<FlushOutcomes, StorageError> {
        let driver = self.driver.take().expect("driver handle");
        let shared = Arc::clone(&self.shared);
        // The entire shutdown sequence is owned by this spawned task,
        // created before the method's first await: signal close, join the
        // driver, quiesce the detached foreground commands, then drain
        // both streams. Dropping or aborting this `close` future only
        // detaches the task — never cancels it — so the shared state (and
        // the writer lease through it) is held exactly until the drain is
        // truly done.
        join_command(tokio::spawn(async move {
            shared.closing.store(true, Ordering::Release);
            shared.opened.notify_one();
            // Join the driver before touching state: its in-flight flush
            // (if any) finishes under the state lock, and once joined it
            // can no longer race the final drain below.
            let _ = driver.await;
            // Quiesce the foreground commands whose callers were cancelled
            // mid-operation: their detached persistence work must land
            // before the drain reads the pending state, and neither it nor
            // the lease may release ahead of them.
            shared.wait_for_commands_to_quiesce().await;
            let mut state = shared.state.lock().await;
            // Both streams are attempted even when the first drain fails
            // (the isolation of `flush_command`); the first error —
            // normalized before raw — is reported deterministically.
            let normalized = flush_normalized(&shared, &mut state).await;
            let raw = flush_raw(&shared, &mut state).await;
            let normalized = normalized?;
            let raw = raw?;
            Ok(FlushOutcomes {
                normalized: final_outcome(normalized, shared.last_outcome(LogStream::Normalized)),
                raw: final_outcome(raw, shared.last_outcome(LogStream::Raw)),
            })
        }))
        .await
    }

    /// Seal the active segment of one stream (flushing that stream's
    /// pending batch into it first, so buffered data lands in the segment
    /// being sealed); that stream's next append creates a fresh one. The
    /// other stream is untouched. Cancellation-safe by construction, like
    /// every foreground operation (see the module docs). Rotation at the
    /// 4 MiB bound is the production seal path; an explicit seal is an
    /// ops/test hook, so the method stays behind test hooks until a
    /// production caller needs it.
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn seal(&mut self, stream: LogStream) -> Result<(), StorageError> {
        let shared = Arc::clone(&self.shared);
        let guard = shared.enter_command();
        join_command(tokio::spawn(async move {
            let _registered = guard;
            seal_command(shared, stream).await
        }))
        .await
    }

    /// Override the 4 MiB rotation threshold (test builds only, so the
    /// retention budget can be exercised without materializing hundreds
    /// of mebibytes; the production value stays
    /// [`crate::MAX_SEGMENT_BYTES`]).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub async fn set_rotation_threshold(&self, bytes: u64) {
        self.shared.state.lock().await.max_segment_bytes = bytes;
    }

    /// Park the next flushed batch once at `point` until the future
    /// completes (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub async fn set_park_once(&self, at: ParkPoint, fut: ParkFuture) {
        self.shared.state.lock().await.park = Some((at, fut));
    }

    /// Fail the next flushed batch at `site` (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub async fn set_fault_once(&self, site: FaultSite) {
        self.shared.state.lock().await.fault = Some(site);
    }

    /// Install a failpoint observer on the shared store (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn set_crash_sink(&self, sink: Option<crate::crash::CrashSink>) {
        self.shared.store.set_crash_sink(sink);
    }
}

fn outcome_of(flush: StreamFlushOutcome) -> AppendOutcome {
    match flush {
        StreamFlushOutcome::Nothing | StreamFlushOutcome::Committed => AppendOutcome::Committed,
        StreamFlushOutcome::Dropped => AppendOutcome::Dropped,
    }
}

/// Await one spawned writer command. The commands are spawned — never
/// aborted — so this join is the caller's window onto work that completes
/// either way: dropping or aborting the awaiting future only detaches the
/// task, which keeps the shared state (and the writer lease) until its
/// Tokio fs/SQLite work truly completes. A join failure is therefore only
/// a panic inside storage code, surfaced as a typed fault instead of
/// being swallowed.
async fn join_command<T>(
    command: tokio::task::JoinHandle<Result<T, StorageError>>,
) -> Result<T, StorageError> {
    match command.await {
        Ok(result) => result,
        Err(error) => Err(StorageError::Database(format!(
            "a writer command task did not finish: {error}"
        ))),
    }
}

/// The body of [`LogWriter::append_line`]: acceptance plus any size-bound
/// flush of one normalized line, under one state lock, inside the command
/// task that owns the lease for the operation's whole duration.
async fn append_line_command(
    shared: Arc<WriterShared>,
    line: u64,
    text: String,
) -> Result<AppendedLine, StorageError> {
    let mut state = shared.state.lock().await;
    if let Some(detail) = &state.normalized.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let watermark = shared.line_watermark.load(Ordering::Relaxed);
    match watermark.checked_add(1) {
        Some(next) if next == line => {}
        _ => {
            return Err(StorageError::LineNotSequential {
                attempted: line,
                watermark,
            });
        }
    }
    state.pending_lines.push((line, text));
    if state.norm_deadline.is_none() {
        state.norm_deadline = Some(Instant::now() + FLUSH_MAX_DELAY);
        shared.opened.notify_one();
    }
    shared.line_watermark.store(line, Ordering::Relaxed);
    let pending_bytes: usize = state.pending_lines.iter().map(|(_, t)| t.len()).sum();
    let outcome = if pending_bytes >= FLUSH_MAX_BYTES {
        outcome_of(flush_normalized(&shared, &mut state).await?)
    } else {
        AppendOutcome::Buffered
    };
    Ok(AppendedLine { line, outcome })
}

/// The body of [`LogWriter::append_raw`]: acceptance plus any size-bound
/// flush of one raw append, under one state lock, inside the command task
/// that owns the lease for the operation's whole duration.
async fn append_raw_command(
    shared: Arc<WriterShared>,
    bytes: Vec<u8>,
) -> Result<AppendedRaw, StorageError> {
    let mut state = shared.state.lock().await;
    if let Some(detail) = &state.raw.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let start_offset = shared.raw_watermark.load(Ordering::Relaxed);
    let append_len = u64::try_from(bytes.len())
        .map_err(|_| StorageError::Database("raw append length overflow".into()))?;
    let end_offset = start_offset
        .checked_add(append_len)
        .ok_or_else(|| StorageError::Database("raw byte offset space exhausted".into()))?;
    if state.pending_raw.is_empty() {
        state.pending_raw_start = start_offset;
    }
    state.pending_raw.extend_from_slice(&bytes);
    if state.raw_deadline.is_none() {
        state.raw_deadline = Some(Instant::now() + FLUSH_MAX_DELAY);
        shared.opened.notify_one();
    }
    shared.raw_watermark.store(end_offset, Ordering::Relaxed);
    let outcome = if state.pending_raw.len() >= FLUSH_MAX_BYTES {
        outcome_of(flush_raw(&shared, &mut state).await?)
    } else {
        AppendOutcome::Buffered
    };
    Ok(AppendedRaw {
        offset: start_offset,
        len: append_len,
        outcome,
    })
}

/// The body of [`LogWriter::record_raw_loss`]: commit any pending raw
/// batch, seal the current raw segment so the loss lands on a segment
/// boundary, then record the explicit gap and advance the non-reusable
/// raw watermark — all under one state lock, inside the command task that
/// owns the lease. Sealing first is what keeps the raw stream's frames
/// offset-contiguous inside every segment: a hole inside a segment would
/// be scanned as corruption after a restart, so the loss is recorded
/// between segments instead.
async fn record_raw_loss_command(
    shared: Arc<WriterShared>,
    len: u64,
) -> Result<AppendedLoss, StorageError> {
    let mut state = shared.state.lock().await;
    if let Some(detail) = &state.raw.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let start = shared.raw_watermark.load(Ordering::Relaxed);
    if len == 0 {
        return Ok(AppendedLoss {
            offset: start,
            len: 0,
        });
    }
    let end = start
        .checked_add(len)
        .ok_or_else(|| StorageError::Database("raw byte offset space exhausted".into()))?;
    // Flush the accepted raw bytes into the current segment, then seal it,
    // so the discarded range is not covered by any segment's indexed range.
    flush_raw(&shared, &mut state).await?;
    seal_active(&shared, &mut state, LogStream::Raw).await?;
    shared
        .store
        .drop_batch(
            &shared.key,
            LogStream::Raw,
            GapSpan {
                start,
                end,
                reason: GapReason::Missing,
            },
            end,
        )
        .await?;
    state.pending_raw_start = end;
    state.raw_deadline = None;
    shared.raw_watermark.store(end, Ordering::Relaxed);
    shared.record_outcome(LogStream::Raw, StreamFlushOutcome::Dropped);
    Ok(AppendedLoss { offset: start, len })
}

/// The body of [`LogWriter::record_line_loss`]: commit the accepted lines
/// into their segment, seal it, then record the retired line numbers as an
/// explicit gap with the watermark advanced past them.
async fn record_line_loss_command(
    shared: Arc<WriterShared>,
    first_line: u64,
    lines: u64,
) -> Result<AppendedLineLoss, StorageError> {
    let mut state = shared.state.lock().await;
    if let Some(detail) = &state.normalized.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let first = shared
        .line_watermark
        .load(Ordering::Relaxed)
        .checked_add(1)
        .ok_or_else(|| StorageError::Database("line number space exhausted".into()))?;
    if first_line != first {
        // A loss consumes numbers; it never skips one, exactly like an
        // append.
        return Err(StorageError::LineNotSequential {
            attempted: first_line,
            watermark: first.saturating_sub(1),
        });
    }
    if lines == 0 {
        return Ok(AppendedLineLoss {
            first_line: first,
            lines: 0,
        });
    }
    let last = first
        .checked_add(lines)
        .ok_or_else(|| StorageError::Database("line number space exhausted".into()))?
        - 1;
    // Flush the accepted lines into the current segment, then seal it, so
    // the discarded range is not covered by any segment's indexed range.
    flush_normalized(&shared, &mut state).await?;
    seal_active(&shared, &mut state, LogStream::Normalized).await?;
    shared
        .store
        .drop_batch(
            &shared.key,
            LogStream::Normalized,
            GapSpan {
                start: first,
                end: last + 1,
                reason: GapReason::Missing,
            },
            last,
        )
        .await?;
    state.norm_deadline = None;
    shared.line_watermark.store(last, Ordering::Relaxed);
    // The last *batch* outcome of the normalized stream stays observable: a
    // retired range is not a batch, and reporting it as one would make a
    // caller believe the pending batch was dropped when it was in fact
    // committed just above. The loss is observable through its return value
    // and the terminal's latches.
    Ok(AppendedLineLoss {
        first_line: first,
        lines,
    })
}

/// The body of [`LogWriter::flush`]: both streams' batches flushed under
/// one state lock, inside the command task that owns the lease for the
/// whole operation.
async fn flush_command(shared: Arc<WriterShared>) -> Result<FlushOutcomes, StorageError> {
    let mut state = shared.state.lock().await;
    let normalized = flush_normalized(&shared, &mut state).await;
    let raw = flush_raw(&shared, &mut state).await;
    let normalized = normalized?;
    let raw = raw?;
    Ok(FlushOutcomes { normalized, raw })
}

/// The body of [`LogWriter::seal`]: the stream's pending batch flushed
/// into the segment being sealed, then that segment's file sync and row
/// seal, inside the command task that owns the lease for the whole
/// operation. A seal failure latches the stream through [`seal_active`]
/// (the active segment is restored, not dropped: its row is still the
/// stream's active segment, and a retry without recovery must not
/// create a second one behind it).
#[cfg(any(test, feature = "test-hooks"))]
async fn seal_command(shared: Arc<WriterShared>, stream: LogStream) -> Result<(), StorageError> {
    let mut state = shared.state.lock().await;
    match stream {
        LogStream::Normalized => flush_normalized(&shared, &mut state).await?,
        LogStream::Raw => flush_raw(&shared, &mut state).await?,
    };
    seal_active(&shared, &mut state, stream).await
}

/// The outcome `close` reports for one stream: the final drain's outcome,
/// or — when the drain found nothing pending — the most recent
/// batch-bearing outcome recorded for that stream, so a deadline flush
/// the driver already committed (or durably dropped) is still reported
/// to the owner instead of a false `Nothing`.
fn final_outcome(drained: StreamFlushOutcome, stored: StreamFlushOutcome) -> StreamFlushOutcome {
    if drained == StreamFlushOutcome::Nothing {
        stored
    } else {
        drained
    }
}

/// Latch one stream recovery-required after a flush failed post-
/// extraction, unless the flush already latched it with a richer detail
/// (the frame-append latch that knows which file holds the tail).
fn poison_after_flush_failure(state: &mut WriterState, stream: LogStream, error: &StorageError) {
    let stream_state = stream_state(state, stream);
    if stream_state.recovery_required.is_some() {
        return;
    }
    stream_state.recovery_required = Some(format!(
        "{} stream refused until recovery after a failed flush ({error}); the accepted \
         batch was not committed, so its numbers must be re-appended — a retry may \
         never silently skip them",
        stream.db_text()
    ));
}

fn stream_state(state: &mut WriterState, stream: LogStream) -> &mut StreamState {
    match stream {
        LogStream::Normalized => &mut state.normalized,
        LogStream::Raw => &mut state.raw,
    }
}

fn stream_state_ref(state: &WriterState, stream: LogStream) -> &StreamState {
    match stream {
        LogStream::Normalized => &state.normalized,
        LogStream::Raw => &state.raw,
    }
}

/// The internal flush driver: enforces the 50 ms half of the batch
/// policy so a trailing batch commits within the delay even when no
/// further append arrives, and records each flush's durable outcome so
/// an owner can observe a driver-initiated drop (the producer keeps
/// draining; the next append, flush, or seal surfaces an error). Signalled
/// closed (never aborted) when the writer drops without `close`: an
/// in-flight blocking-pool file operation cannot be cancelled, so the
/// driver — and the lease it holds — always outlives the operation it
/// started. A flush error latches the stream recovery-required instead of
/// being swallowed.
async fn drive(shared: Arc<WriterShared>) {
    loop {
        if shared.closing.load(Ordering::Acquire) {
            return;
        }
        let deadline = {
            let state = shared.state.lock().await;
            [state.norm_deadline, state.raw_deadline]
                .into_iter()
                .flatten()
                .min()
        };
        match deadline {
            Some(deadline) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {}
                    _ = shared.opened.notified() => {}
                }
            }
            None => {
                shared.opened.notified().await;
            }
        }
        if shared.closing.load(Ordering::Acquire) {
            return;
        }
        let mut state = shared.state.lock().await;
        let now = Instant::now();
        if state.norm_deadline.is_some_and(|at| at <= now) {
            let _ = flush_normalized(&shared, &mut state).await;
        }
        if state.raw_deadline.is_some_and(|at| at <= now) {
            let _ = flush_raw(&shared, &mut state).await;
        }
    }
}

/// Flush the normalized stream's pending batch through the full commit
/// sequence. See the module docs for the ordering and the failure latch.
/// The batch is extracted up front; **every** failure after that point —
/// rotation, segment creation, header write/sync, directory sync, frame
/// append, file sync, visibility transaction — latches the stream
/// recovery-required, so a retry can never silently skip the accepted
/// range (its numbers stay unconsumed until the caller re-appends them
/// after recovery).
async fn flush_normalized(
    shared: &WriterShared,
    state: &mut WriterState,
) -> Result<StreamFlushOutcome, StorageError> {
    if let Some(detail) = &state.normalized.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let lines = std::mem::take(&mut state.pending_lines);
    state.norm_deadline = None;
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return Ok(StreamFlushOutcome::Nothing);
    };
    match flush_normalized_batch(shared, state, &lines, first.0, last.0).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            poison_after_flush_failure(state, LogStream::Normalized, &error);
            Err(error)
        }
    }
}

/// The extracted normalized batch: rotation, segment creation, frame
/// append, sync, and the visibility transaction.
async fn flush_normalized_batch(
    shared: &WriterShared,
    state: &mut WriterState,
    lines: &[(u64, String)],
    first_line: u64,
    last_line: u64,
) -> Result<StreamFlushOutcome, StorageError> {
    // Encoded size only (per frame: header + payload + crc32): the
    // rotation decision needs it before the frames are built, because a
    // fresh segment restarts the frame sequence at 0.
    let frame_overhead = (crate::frame::FRAME_HEADER_LEN + 4) as u64;
    let buf_len: u64 = lines
        .iter()
        .map(|(_, text)| {
            split_line(text)
                .iter()
                .map(|chunk| chunk.len() as u64 + frame_overhead)
                .sum::<u64>()
        })
        .sum();

    // Rotation before writing if this batch would overflow the segment.
    if needs_rotation(state, LogStream::Normalized, buf_len) {
        seal_active(shared, state, LogStream::Normalized).await?;
    }
    if state.normalized.active.is_none() {
        match create_segment(shared, state, LogStream::Normalized, Some(first_line), 0).await? {
            CreateOutcome::Created => {}
            CreateOutcome::UnsafeReclaim => {
                drop_batch(
                    shared,
                    LogStream::Normalized,
                    first_line,
                    last_line.checked_add(1).ok_or_else(|| {
                        StorageError::Database("line number space exhausted".into())
                    })?,
                    last_line,
                )
                .await?;
                shared.record_outcome(LogStream::Normalized, StreamFlushOutcome::Dropped);
                return Ok(StreamFlushOutcome::Dropped);
            }
        }
    }
    // Compare-and-set expectations of this commit: the terminal's
    // watermark before the batch and the receiving segment's indexed
    // range end. Both are exactly what this writer last published, so a
    // commit can only fail them if someone else moved the durable state.
    let prior_line_watermark = first_line
        .checked_sub(1)
        .ok_or_else(|| StorageError::Database("line numbering must start at 1".into()))?;
    let prior_segment_last_line = state
        .normalized
        .active
        .as_ref()
        .and_then(|active| active.row.last_line)
        .ok_or_else(|| {
            StorageError::Database("normalized segment row carries no indexed range".into())
        })?;

    // Build the frames only now, with the receiving segment's frame
    // sequence (a fresh segment starts at 0; the scanner requires it).
    let mut buf = Vec::with_capacity(buf_len as usize);
    let mut seq = state.normalized.next_frame_seq;
    for (line, text) in lines {
        let chunks = split_line(text);
        let mut offset = 0u64;
        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i + 1 == chunks.len();
            let header = FrameHeader {
                kind: FRAME_KIND_LINE,
                flags: if is_last { FRAME_FLAG_LINE_END } else { 0 },
                frame_seq: seq,
                line: *line,
                line_offset: offset,
                payload_len: u32::try_from(chunk.len()).expect("chunk bounded by MAX_PAYLOAD"),
            };
            buf.extend_from_slice(
                &encode_frame(&header, chunk)
                    .map_err(|error| StorageError::Database(format!("encode frame: {error:?}")))?,
            );
            offset += chunk.len() as u64;
            seq = seq
                .checked_add(1)
                .ok_or(StorageError::Database("frame_seq exhausted".into()))?;
        }
    }

    let (segment_id, new_len) =
        write_and_sync_frames(shared, state, LogStream::Normalized, &buf).await?;
    #[cfg(any(test, feature = "test-hooks"))]
    if state.fault.take() == Some(FaultSite::Commit) {
        return Err(StorageError::Database(
            "injected visibility-transaction failure".into(),
        ));
    }
    let exclusive_end = last_line
        .checked_add(1)
        .ok_or_else(|| StorageError::Database("line number space exhausted".into()))?;
    // 3. short SQLite transaction publishing the state
    shared
        .store
        .commit_visible(
            &shared.key,
            &CommitInput {
                segment_id,
                committed_bytes: new_len,
                fsynced_bytes: new_len,
                commit: StreamCommit::Normalized {
                    segment_last_line: exclusive_end,
                    line_watermark: last_line,
                    prior_segment_last_line,
                    prior_line_watermark,
                },
            },
        )
        .await?;
    shared.store.hit(CrashPoint::PublishBefore);
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some((_, fut)) = state.park.take_if(|(at, _)| *at == ParkPoint::AfterCommit) {
        fut.await;
    }
    // 4. publish only now
    if let Some(active) = state.normalized.active.as_mut() {
        active.len = new_len;
        active.row.committed_bytes = new_len;
        active.row.fsynced_bytes = new_len;
        // Keep the cached indexed range current: it is the next batch's
        // compare-and-set expectation.
        active.row.last_line = Some(exclusive_end);
    }
    state.normalized.next_frame_seq = seq;
    state.normalized.recovery_required = None;
    shared.line_watermark.store(last_line, Ordering::Relaxed);
    shared.record_outcome(LogStream::Normalized, StreamFlushOutcome::Committed);
    shared.store.hit(CrashPoint::PublishAfter);
    Ok(StreamFlushOutcome::Committed)
}

/// Flush the raw stream's pending batch through the full commit
/// sequence. Raw frames are byte-addressed: the stream offset rides in
/// `line_offset`, `line` stays 0, and no line flags are set, so a raw
/// frame can never be mistaken for a normalized line frame. The batch is
/// extracted up front and every failure after that point latches the
/// stream recovery-required (see [`flush_normalized`]).
async fn flush_raw(
    shared: &WriterShared,
    state: &mut WriterState,
) -> Result<StreamFlushOutcome, StorageError> {
    if let Some(detail) = &state.raw.recovery_required {
        return Err(StorageError::RecoveryRequired {
            detail: detail.clone(),
        });
    }
    let bytes = std::mem::take(&mut state.pending_raw);
    state.raw_deadline = None;
    if bytes.is_empty() {
        return Ok(StreamFlushOutcome::Nothing);
    }
    match flush_raw_batch(shared, state, &bytes, state.pending_raw_start).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            poison_after_flush_failure(state, LogStream::Raw, &error);
            Err(error)
        }
    }
}

/// The extracted raw batch: rotation, segment creation, frame append,
/// sync, and the visibility transaction.
async fn flush_raw_batch(
    shared: &WriterShared,
    state: &mut WriterState,
    bytes: &[u8],
    start_offset: u64,
) -> Result<StreamFlushOutcome, StorageError> {
    let append_len = bytes.len() as u64;
    let end_offset = start_offset
        .checked_add(append_len)
        .ok_or_else(|| StorageError::Database("raw byte offset space exhausted".into()))?;
    let chunks = split_raw(bytes);
    let frame_overhead = (crate::frame::FRAME_HEADER_LEN + 4) as u64;
    let buf_len: u64 = chunks
        .iter()
        .map(|chunk| chunk.len() as u64 + frame_overhead)
        .sum();

    if needs_rotation(state, LogStream::Raw, buf_len) {
        seal_active(shared, state, LogStream::Raw).await?;
    }
    if state.raw.active.is_none() {
        match create_segment(shared, state, LogStream::Raw, None, start_offset).await? {
            CreateOutcome::Created => {}
            CreateOutcome::UnsafeReclaim => {
                drop_batch(shared, LogStream::Raw, start_offset, end_offset, end_offset).await?;
                shared.record_outcome(LogStream::Raw, StreamFlushOutcome::Dropped);
                return Ok(StreamFlushOutcome::Dropped);
            }
        }
    }
    // Compare-and-set expectations of this commit (see
    // `flush_normalized_batch`).
    let prior_raw_watermark = start_offset;
    let prior_segment_last_offset = state
        .raw
        .active
        .as_ref()
        .map(|active| active.row.last_offset)
        .ok_or_else(|| StorageError::Database("raw segment row carries no indexed range".into()))?;

    let mut buf = Vec::with_capacity(buf_len as usize);
    let mut seq = state.raw.next_frame_seq;
    let mut offset = start_offset;
    for chunk in &chunks {
        let header = FrameHeader {
            kind: FRAME_KIND_RAW,
            flags: 0,
            frame_seq: seq,
            line: 0,
            line_offset: offset,
            payload_len: u32::try_from(chunk.len()).expect("chunk bounded by MAX_PAYLOAD"),
        };
        buf.extend_from_slice(
            &encode_frame(&header, chunk)
                .map_err(|error| StorageError::Database(format!("encode frame: {error:?}")))?,
        );
        offset = offset
            .checked_add(chunk.len() as u64)
            .ok_or(StorageError::Database(
                "raw byte offset space exhausted".into(),
            ))?;
        seq = seq
            .checked_add(1)
            .ok_or(StorageError::Database("frame_seq exhausted".into()))?;
    }

    let (segment_id, new_len) = write_and_sync_frames(shared, state, LogStream::Raw, &buf).await?;
    #[cfg(any(test, feature = "test-hooks"))]
    if state.fault.take() == Some(FaultSite::Commit) {
        return Err(StorageError::Database(
            "injected visibility-transaction failure".into(),
        ));
    }
    shared
        .store
        .commit_visible(
            &shared.key,
            &CommitInput {
                segment_id,
                committed_bytes: new_len,
                fsynced_bytes: new_len,
                commit: StreamCommit::Raw {
                    segment_last_offset: end_offset,
                    raw_watermark: end_offset,
                    prior_segment_last_offset,
                    prior_raw_watermark,
                },
            },
        )
        .await?;
    shared.store.hit(CrashPoint::PublishBefore);
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some((_, fut)) = state.park.take_if(|(at, _)| *at == ParkPoint::AfterCommit) {
        fut.await;
    }
    if let Some(active) = state.raw.active.as_mut() {
        active.len = new_len;
        active.row.committed_bytes = new_len;
        active.row.fsynced_bytes = new_len;
        // Keep the cached indexed range current: it is the next batch's
        // compare-and-set expectation.
        active.row.last_offset = end_offset;
    }
    state.raw.next_frame_seq = seq;
    state.raw.recovery_required = None;
    shared.raw_watermark.store(end_offset, Ordering::Relaxed);
    shared.record_outcome(LogStream::Raw, StreamFlushOutcome::Committed);
    shared.store.hit(CrashPoint::PublishAfter);
    Ok(StreamFlushOutcome::Committed)
}

/// Steps 1 and 2 of the commit sequence for one stream's active segment:
/// append the encoded frames, then `sync_data`. The failure latch is set
/// *before* the first byte is written and cleared only by the caller
/// after the visibility transaction committed and published, so any
/// write, sync, or commit failure leaves the stream refusing further
/// appends until recovery truncated the tail. Returns the segment id and
/// the file length after the append.
async fn write_and_sync_frames(
    shared: &WriterShared,
    state: &mut WriterState,
    stream: LogStream,
    buf: &[u8],
) -> Result<(i64, u64), StorageError> {
    let file_name = stream_state_ref(state, stream)
        .active
        .as_ref()
        .expect("active segment")
        .row
        .file_name
        .clone();
    let path = paths::segment_path(&shared.root, &file_name);
    let stream_text = stream.db_text();
    stream_state(state, stream).recovery_required = Some(format!(
        "{stream_text} stream has an uncommitted tail after a failed \
         append/sync/commit; run recovery before appending again ({file_name})"
    ));
    #[cfg(any(test, feature = "test-hooks"))]
    // The Commit-site fault belongs to the flush callers (their visibility
    // transaction); only the write/sync sites are consumed here.
    let fault = state.fault.take_if(|site| *site != FaultSite::Commit);
    // 1. append complete frames
    shared.store.hit(CrashPoint::FrameBeforeWrite);
    {
        let active = stream_state(state, stream)
            .active
            .as_mut()
            .expect("active segment");
        #[cfg(any(test, feature = "test-hooks"))]
        if fault == Some(FaultSite::PartialFrameWrite) {
            let half = buf.len() / 2;
            if half > 0 {
                active
                    .file
                    .write_all(&buf[..half])
                    .await
                    .map_err(io_error(&path))?;
            }
            return Err(StorageError::Io {
                path,
                source: std::io::Error::other("injected partial frame write"),
            });
        }
        active.file.write_all(buf).await.map_err(io_error(&path))?;
        // `write_all` on tokio::fs::File hands the bytes to the blocking
        // pool and may resolve before the write syscall ran; `flush`
        // barriers on that in-flight write, so "frames appended" is real
        // when the failpoint below fires (the fsync ordering is unchanged:
        // `sync_data` below also waits for this write before returning).
        active.file.flush().await.map_err(io_error(&path))?;
    }
    shared.store.hit(CrashPoint::FrameAfterWrite);
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some((_, fut)) = state
        .park
        .take_if(|(at, _)| *at == ParkPoint::AfterFrameWrite)
    {
        fut.await;
    }
    // 2. sync file data (header and parent dir were synced at creation)
    {
        let active = stream_state(state, stream)
            .active
            .as_mut()
            .expect("active segment");
        #[cfg(any(test, feature = "test-hooks"))]
        if fault == Some(FaultSite::FrameSync) {
            return Err(StorageError::Io {
                path,
                source: std::io::Error::other("injected file sync failure"),
            });
        }
        active.file.sync_data().await.map_err(io_error(&path))?;
    }
    shared.store.hit(CrashPoint::FrameAfterSync);
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some((_, fut)) = state
        .park
        .take_if(|(at, _)| *at == ParkPoint::AfterFileSync)
    {
        fut.await;
    }
    let active = stream_state_ref(state, stream)
        .active
        .as_ref()
        .expect("active segment");
    Ok((active.row.segment_id, active.len + buf.len() as u64))
}

/// Whether the stream's active segment must be sealed before appending
/// `buf_len` more bytes (rotation at 4 MiB, per stream).
fn needs_rotation(state: &WriterState, stream: LogStream, buf_len: u64) -> bool {
    let Some(active) = stream_state_ref(state, stream).active.as_ref() else {
        return false;
    };
    active.row.state == "active"
        && active.len > SEGMENT_HEADER_LEN as u64
        && active.len + buf_len > state.max_segment_bytes
}

/// Seal the active segment of one stream (its pending batch must already
/// be flushed into it): the file's `sync_data`, then the segment row's
/// seal update. Both steps are fallible, so the active segment is only
/// dropped once its row is sealed — on any failure it is restored into
/// the stream state and the stream is latched recovery-required: the row
/// is still this stream's active segment, and a retry without recovery
/// could otherwise create a second active segment behind it. Rotation
/// callers surface the same error as their flush failure (the latch then
/// deduplicates with `poison_after_flush_failure`).
async fn seal_active(
    shared: &WriterShared,
    state: &mut WriterState,
    stream: LogStream,
) -> Result<(), StorageError> {
    let Some(active) = stream_state(state, stream).active.take() else {
        return Ok(());
    };
    match sync_and_seal(shared, state, &active).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let stream_state = stream_state(state, stream);
            stream_state.active = Some(active);
            if stream_state.recovery_required.is_none() {
                stream_state.recovery_required = Some(format!(
                    "{} stream refused until recovery after a failed seal ({error}); its active \
                     segment is unchanged, so a retry must not create a second segment behind it",
                    stream.db_text()
                ));
            }
            Err(error)
        }
    }
}

/// The fallible steps of one seal: the active segment file's
/// `sync_data`, then the segment row's seal update. Test-hook faults:
/// [`FaultSite::SealSync`] fails the file sync, [`FaultSite::SealDb`]
/// fails the row update after a successful sync.
async fn sync_and_seal(
    shared: &WriterShared,
    #[cfg_attr(not(any(test, feature = "test-hooks")), allow(unused_variables))]
    state: &mut WriterState,
    active: &ActiveSegment,
) -> Result<(), StorageError> {
    let path = paths::segment_path(&shared.root, &active.row.file_name);
    #[cfg(any(test, feature = "test-hooks"))]
    if state
        .fault
        .take_if(|site| *site == FaultSite::SealSync)
        .is_some()
    {
        return Err(StorageError::Io {
            path,
            source: std::io::Error::other("injected seal file sync failure"),
        });
    }
    active.file.sync_data().await.map_err(io_error(&path))?;
    #[cfg(any(test, feature = "test-hooks"))]
    if state
        .fault
        .take_if(|site| *site == FaultSite::SealDb)
        .is_some()
    {
        return Err(StorageError::Database(
            "injected segment row seal failure".into(),
        ));
    }
    shared.store.seal_segment(active.row.segment_id).await?;
    Ok(())
}

/// Record one dropped batch: the stream-scoped gap for its exact range,
/// the non-reusable watermark advanced past it, and the `degraded` +
/// `refuse_new_start` latches — one transaction, so a crash in between
/// leaves either nothing or the whole recorded drop, never a silent
/// loss. The reason is `missing`: no segment ever backed the range.
async fn drop_batch(
    shared: &WriterShared,
    stream: LogStream,
    start: u64,
    end: u64,
    watermark: u64,
) -> Result<(), StorageError> {
    shared
        .store
        .drop_batch(
            &shared.key,
            stream,
            GapSpan {
                start,
                end,
                reason: GapReason::Missing,
            },
            watermark,
        )
        .await
}

/// Create a fresh segment of one stream: row first (for the id the
/// header must carry), then the file — created *exclusively*
/// (`create_new`), so an unrecovered file occupying the name (an
/// old-epoch leftover after a destructive database rebuild, or any
/// unclaimed orphan) refuses with [`StorageError::RecoveryRequired`]
/// instead of being appended to and adopted — then sync the header and
/// the parent directory before any frame is written. The creation runs
/// inside the per-terminal metadata budget: when it would exceed
/// [`crate::MAX_SEGMENT_METADATA_ROWS`] live rows across both streams,
/// the creation transaction reclaims the oldest sealed segment first and
/// this function unlinks the reclaimed file after the transaction
/// committed (row-before-file ordering, so a crash in between leaves a
/// reclaimable orphan, never a fabricated loss). When no sealed segment
/// can be reclaimed safely the creation is refused without mutation and
/// the caller drops its batch instead ([`SegmentCreation::UnsafeReclaim`]).
///
/// Creation-order divergence from the Gate C probe (documented
/// deliberately): the probe wrote file + header + dir fsync *before*
/// the DB row (orphan file -> discover + quarantine), while production
/// inserts the row first because the header must carry the DB-assigned
/// segment id. The recovery pass therefore handles both orphan
/// classes: a row without its file, and a file without its row.
async fn create_segment(
    shared: &WriterShared,
    state: &mut WriterState,
    stream: LogStream,
    first_line: Option<u64>,
    first_offset: u64,
) -> Result<CreateOutcome, StorageError> {
    shared.store.hit(CrashPoint::SegmentBefore);
    let inserted = match shared
        .store
        .insert_segment(&shared.key, stream, first_line, first_offset)
        .await?
    {
        SegmentCreation::Created(inserted) => inserted,
        SegmentCreation::UnsafeReclaim => return Ok(CreateOutcome::UnsafeReclaim),
    };
    shared.store.hit(CrashPoint::SegmentRowInserted);
    if let Some(reclaimed) = &inserted.reclaimed_file_name {
        let path = paths::segment_path(&shared.root, reclaimed);
        #[cfg(any(test, feature = "test-hooks"))]
        if state
            .fault
            .take_if(|site| *site == FaultSite::ReclaimUnlink)
            .is_some()
        {
            return Err(StorageError::Io {
                path,
                source: std::io::Error::other("injected reclaimed-segment unlink failure"),
            });
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(StorageError::Io { path, source }),
        }
        #[cfg(any(test, feature = "test-hooks"))]
        if state
            .fault
            .take_if(|site| *site == FaultSite::DirSync)
            .is_some()
        {
            return Err(StorageError::Io {
                path: shared.root.clone(),
                source: std::io::Error::other("injected directory fsync failure"),
            });
        }
        paths::fsync_dir(&shared.root).await?;
    }
    let segment_id = inserted.segment_id;
    let file_name = inserted.file_name;
    let header = SegmentHeader {
        kind: stream.segment_kind(),
        flags: 0,
        terminal: shared.identity.terminal_uuid,
        epoch: shared.identity.epoch,
        segment_id: u64::try_from(segment_id)
            .map_err(|_| StorageError::Database("segment id overflow".into()))?,
        created_ms: paths::now_ms(),
    };
    let path = paths::segment_path(&shared.root, &file_name);
    // Exclusive creation: a name collision means an unrecovered file owns
    // these bytes, and adopting them (or appending behind them) would
    // expose unindexed data. Require the recovery pass first.
    let mut file = match tokio::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(StorageError::RecoveryRequired {
                detail: format!(
                    "segment file {file_name} already exists but is not this writer's; \
                     run recovery to quarantine unclaimed files before writing"
                ),
            });
        }
        Err(source) => return Err(StorageError::Io { path, source }),
    };
    #[cfg(any(test, feature = "test-hooks"))]
    if state
        .fault
        .take_if(|site| *site == FaultSite::HeaderWrite)
        .is_some()
    {
        return Err(StorageError::Io {
            path: path.clone(),
            source: std::io::Error::other("injected segment header write failure"),
        });
    }
    file.write_all(&header.encode())
        .await
        .map_err(io_error(&path))?;
    // Same barrier as the frame append: the header bytes must be in the
    // OS when the "written, not yet synced" failpoint fires.
    file.flush().await.map_err(io_error(&path))?;
    shared.store.hit(CrashPoint::SegmentHeaderWritten);
    #[cfg(any(test, feature = "test-hooks"))]
    if state
        .fault
        .take_if(|site| *site == FaultSite::HeaderSync)
        .is_some()
    {
        return Err(StorageError::Io {
            path: path.clone(),
            source: std::io::Error::other("injected segment header sync failure"),
        });
    }
    file.sync_data().await.map_err(io_error(&path))?;
    #[cfg(any(test, feature = "test-hooks"))]
    if state
        .fault
        .take_if(|site| *site == FaultSite::DirSync)
        .is_some()
    {
        return Err(StorageError::Io {
            path: shared.root.clone(),
            source: std::io::Error::other("injected directory fsync failure"),
        });
    }
    paths::fsync_dir(&shared.root).await?;
    shared.store.hit(CrashPoint::SegmentHeaderSynced);
    let row = shared.store.segment(segment_id).await?;
    let stream_state = stream_state(state, stream);
    stream_state.active = Some(ActiveSegment {
        row,
        file,
        len: SEGMENT_HEADER_LEN as u64,
    });
    stream_state.next_frame_seq = 0;
    Ok(CreateOutcome::Created)
}

/// Attach the active segment row of one stream (if any) and rescan its
/// committed prefix to recover the frame sequence position. The file
/// length must equal the committed boundary and its frames must agree
/// with the row's indexed range; anything else needs the recovery pass
/// (asserted here, not repaired — S2b).
async fn attach_active_segment(
    shared: &WriterShared,
    term: &TerminalRow,
    stream: LogStream,
) -> Result<(), StorageError> {
    let segment_id = match stream {
        LogStream::Normalized => term.active_normalized_segment,
        LogStream::Raw => term.active_raw_segment,
    };
    let Some(segment_id) = segment_id else {
        return Ok(());
    };
    let row = shared.store.segment(segment_id).await?;
    if row.state != "active" {
        return Ok(()); // sealed/quarantined/missing: a fresh segment is created on append
    }
    if row.kind != stream {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "active {} segment pointer references a {} segment row",
                stream.db_text(),
                row.kind.db_text()
            ),
        });
    }
    let path = paths::segment_path(&shared.root, &row.file_name);
    let data = match tokio::fs::read(&path).await {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StorageError::RecoveryRequired {
                detail: format!("active segment file {} is missing", row.file_name),
            });
        }
        Err(source) => return Err(StorageError::Io { path, source }),
    };
    // The file must end exactly at the committed boundary; the one
    // exception is a segment whose creation committed nothing yet —
    // its boundary is the synced header itself.
    let boundary = row.committed_bytes.max(SEGMENT_HEADER_LEN as u64);
    if data.len() as u64 != boundary {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "active segment {}: file is {} bytes but committed boundary is {} \
                 (uncommitted tail or truncation); run recovery",
                row.file_name,
                data.len(),
                row.committed_bytes
            ),
        });
    }
    let header = SegmentHeader::parse(&data).map_err(|error| StorageError::RecoveryRequired {
        detail: format!(
            "active segment {} header is invalid: {error}",
            row.file_name
        ),
    })?;
    if header.kind != stream.segment_kind() {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "active segment {} header kind does not match its row kind",
                row.file_name
            ),
        });
    }
    if header.terminal != shared.identity.terminal_uuid
        || header.epoch != shared.identity.epoch
        || header.segment_id != row.segment_id as u64
    {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "active segment {} header identity does not match the terminal row",
                row.file_name
            ),
        });
    }
    let scan = scan_frames(&data, SEGMENT_HEADER_LEN, header.kind);
    if scan.outcome != ScanOutcome::Clean || !frames_match_row(&scan, &row) {
        return Err(StorageError::RecoveryRequired {
            detail: format!(
                "active segment {} committed prefix is not clean or does not match its \
                 indexed range: {:?}",
                row.file_name, scan.outcome
            ),
        });
    }
    let next_frame_seq = match scan
        .frames
        .last()
        .and_then(|f| f.header.frame_seq.checked_add(1))
    {
        Some(next) => next,
        None if scan.frames.is_empty() => 0,
        None => {
            return Err(StorageError::RecoveryRequired {
                detail: format!("active segment {} frame_seq is exhausted", row.file_name),
            });
        }
    };
    let len = data.len() as u64;
    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .read(true)
        .open(&path)
        .await
        .map_err(io_error(&path))?;
    let mut state = shared.state.lock().await;
    let stream_state = stream_state(&mut state, stream);
    stream_state.active = Some(ActiveSegment { row, file, len });
    stream_state.next_frame_seq = next_frame_seq;
    Ok(())
}

impl WriterState {
    /// Synchronously initialize the state (test support for constructing
    /// a [`WriterState`] without a store).
    #[cfg(test)]
    fn new() -> Self {
        WriterState {
            normalized: StreamState {
                active: None,
                next_frame_seq: 0,
                recovery_required: None,
            },
            raw: StreamState {
                active: None,
                next_frame_seq: 0,
                recovery_required: None,
            },
            pending_lines: Vec::new(),
            pending_raw: Vec::new(),
            pending_raw_start: 0,
            norm_deadline: None,
            raw_deadline: None,
            max_segment_bytes: MAX_SEGMENT_BYTES,
            #[cfg(any(test, feature = "test-hooks"))]
            park: None,
            #[cfg(any(test, feature = "test-hooks"))]
            fault: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_policy_constants_are_the_confirmed_production_values() {
        assert_eq!(FLUSH_MAX_DELAY, std::time::Duration::from_millis(50));
        assert_eq!(FLUSH_MAX_BYTES, 64 * 1024);
        assert_eq!(MAX_SEGMENT_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn fresh_state_has_no_pending_batch_or_latch() {
        let state = WriterState::new();
        assert!(state.pending_lines.is_empty());
        assert!(state.pending_raw.is_empty());
        assert!(state.norm_deadline.is_none());
        assert!(state.raw_deadline.is_none());
        assert!(state.normalized.recovery_required.is_none());
        assert!(state.raw.recovery_required.is_none());
    }
}
