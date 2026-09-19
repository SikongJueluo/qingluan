// Throwaway probe-local PTY lifecycle model (probe B), restructured per the
// approved Gate B direction. NOT production code.
//
// Layout:
// - pty-process 0.5.3 owns PTY allocation, spawn (session leader + controlling
//   tty via its own pre_exec, composed with our cgroup-join pre_exec), and the
//   master read side (AsyncRead).
// - Each terminal gets a dedicated cgroup; every descendant (fork, new pgrp,
//   setsid) stays in it. Stop = pidfd-guarded SIGTERM rounds over current
//   cgroup members, then cgroup.kill, then populated==0, root reaped,
//   reader/writer tasks joined, cgroup removed — only then is the quota slot
//   Released.
// - Writes use a duplicated master fd wrapped in tokio AsyncFd with
//   non-blocking try_write. Readiness waits, the write deadline and
//   service-shutdown cancellation all happen OUTSIDE the coordinator lock;
//   the generation/stop barrier check and the actual write syscall happen
//   together inside one short critical section, so every byte and every
//   generation/stop commit is totally ordered by that mutex. The
//   coordinator keeps an ORDERED commit log under that same lock —
//   SwitchCommit(new_generation, seq) for every generation bump and
//   WriteCommit(generation, seq, bytes) for every successful write syscall
//   — so scanning the log in commit order proves the invariant directly:
//   after every SwitchCommit, no WriteCommit with a generation below the
//   current one may appear. A generation change observed BEFORE the commit
//   is a legal rejection, commits no bytes, and never enters the log.
// - Lifecycle events are unique: try_wait errors emit a separate MonitorFault
//   (never a fabricated ProcessExited); OutputClosed commits through a
//   one-shot atomic; concurrent Stop calls share one intent, one completion
//   task that publishes a cloneable Ok/Err result, and one quota release.
// - Startup rollback: every spawned management task enters the rollback
//   guard the moment it exists (the success path takes the handles back
//   out), and the rollback's root-reap failures — a try_wait error or a
//   deadline expiry — are cleanup failures: the slot stays in Cleaning and
//   is never released.
// - Accepted write payloads are normalized to exact-length boxed slices
//   before enqueueing, so the accepted queue never retains the caller
//   Vec's spare capacity.

use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::AsyncReadExt;
use tokio::io::unix::AsyncFd;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::cgroup::{self, ProbeCgroupRoot, TerminalCgroup};
use crate::proc;
use crate::quota::{Quota, QuotaError, SlotState};

/// Probe-local write chunk; the design requires bounded chunks.
pub const WRITE_CHUNK: usize = 4096;
/// Bounded send queue (design: "服务端输入消息与写入分块有界").
pub const WRITE_QUEUE_CAPACITY: usize = 2;
/// Candidate per-message input byte bound (design: "有界输入消息"). A send
/// larger than this is rejected with a typed outcome BEFORE enqueueing, so
/// the accepted queued+inflight bytes are bounded by
/// (WRITE_QUEUE_CAPACITY + 1) * MAX_SEND_BYTES = 768 KiB (queue capacity 2
/// plus the one in-flight write). Accepted payloads are normalized to
/// exact-length boxed slices before enqueueing, so this is a bound on the
/// payload bytes actually backed per accepted message — the caller Vec's
/// spare capacity is never retained; allocator metadata/rounding is outside
/// the bound.
pub const MAX_SEND_BYTES: usize = 256 * 1024;
/// Default per-send write deadline (design: "写入等待有服务端期限").
pub const DEFAULT_WRITE_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum EnvSpec {
    /// Full explicit snapshot; an empty map is legal.
    Snapshot(Vec<(String, String)>),
    /// Explicitly empty environment.
    Empty,
    /// Environment not provided — must be rejected before any allocation.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    StopIntent,
    ControlLost,
    WriteDeadline,
    ServiceShutdown,
    WriteFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEnd {
    Eof,
    Forced,
    ReadError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Complete {
        written: usize,
    },
    Aborted {
        written: usize,
        reason: AbortReason,
    },
    RejectedAfterStop,
    RejectedStaleGeneration,
    RejectedQueueFull,
    /// Larger than `MAX_SEND_BYTES`; rejected before enqueueing.
    RejectedOversize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // StartFailed's message surfaces through Debug formatting
pub enum SpawnError {
    EnvMissing,
    Quota(QuotaError),
    StartFailed(String),
    InjectedFault(&'static str),
    /// Startup rollback could not verify full cleanup (cgroup not empty, a
    /// task join timed out, or the cgroup remove failed). The slot stays in
    /// Cleaning and is never Released; the probe must treat this as a hard
    /// failure.
    CleanupFailed {
        fault: String,
        detail: String,
    },
}

/// Startup fault-injection points for the rollback scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultStep {
    BeforePty,
    AfterPtyOpen,
    AfterSpawn,
    AfterProcStat,
    AfterTaskStart,
    /// Inject one synthetic try_wait error at monitor poll #N (>= 1).
    MonitorPoll(usize),
    /// Fault whose rollback deliberately skips cgroup.kill, so the cleanup
    /// verification (cgroup empty before release) must fail and the slot
    /// must stay in Cleaning.
    RollbackNoKill,
    /// Rollback-phase fault: the root-reap verification observes a synthetic
    /// try_wait error — a cleanup failure (Cleaning, never released).
    RollbackReapError,
    /// Rollback-phase fault: the root is never observed reaped before the
    /// rollback deadline — a cleanup failure (Cleaning, never released).
    RollbackReapTimeout,
}

impl FaultStep {
    pub fn name(self) -> &'static str {
        match self {
            FaultStep::BeforePty => "BeforePty",
            FaultStep::AfterPtyOpen => "AfterPtyOpen",
            FaultStep::AfterSpawn => "AfterSpawn",
            FaultStep::AfterProcStat => "AfterProcStat",
            FaultStep::AfterTaskStart => "AfterTaskStart",
            FaultStep::MonitorPoll(_) => "MonitorPoll",
            FaultStep::RollbackNoKill => "RollbackNoKill",
            FaultStep::RollbackReapError => "RollbackReapError",
            FaultStep::RollbackReapTimeout => "RollbackReapTimeout",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // surfaced through the Debug event log and matchers
pub enum Event {
    Spawned {
        pid: i32,
        cgroup: String,
    },
    ProcessExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// try_wait failed: NOT an exit. `exited` stays false; no fabricated exit.
    MonitorFault {
        message: String,
    },
    OutputClosed {
        end: OutputEnd,
    },
    StopIntentCommitted,
    StopCompleted {
        forced: bool,
        term_phase_ms: u64,
        kill_phase_ms: u64,
        total_ms: u64,
        term_rounds: usize,
        signalled: usize,
    },
    SendComplete {
        written: usize,
    },
    SendAborted {
        written: usize,
        reason: AbortReason,
    },
    SendRejectedAfterStop,
    SendRejectedStaleGeneration,
    SendRejectedQueueFull,
    SendRejectedOversize,
    ResizeApplied {
        rows: u16,
        cols: u16,
    },
    ResizeRejected {
        reason: String,
    },
    QuotaReleased,
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Spawned { .. } => "spawned",
            Event::ProcessExited { .. } => "process-exited",
            Event::MonitorFault { .. } => "monitor-fault",
            Event::OutputClosed { .. } => "output-closed",
            Event::StopIntentCommitted => "stop-intent-committed",
            Event::StopCompleted { .. } => "stop-completed",
            Event::SendComplete { .. } => "send-complete",
            Event::SendAborted { .. } => "send-aborted",
            Event::SendRejectedAfterStop => "send-rejected-after-stop",
            Event::SendRejectedStaleGeneration => "send-rejected-stale-generation",
            Event::SendRejectedQueueFull => "send-rejected-queue-full",
            Event::SendRejectedOversize => "send-rejected-oversize",
            Event::ResizeApplied { .. } => "resize-applied",
            Event::ResizeRejected { .. } => "resize-rejected",
            Event::QuotaReleased => "quota-released",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopStats {
    pub forced: bool,
    pub term_phase_ms: u64,
    pub kill_phase_ms: u64,
    pub total_ms: u64,
    pub term_rounds: usize,
    pub signalled: usize,
    /// Output ended by force-close because the bounded wait expired.
    pub output_forced_close: bool,
    /// True when this is an existing-state result for an already finalized
    /// terminal (no new stop sequence ran).
    pub existing_state: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct StopParams {
    pub term_grace: Duration,
    /// Bounded wait for populated==0 after writing cgroup.kill.
    pub empty_wait: Duration,
    /// Bounded wait for the output side to end before force-closing it.
    pub output_wait: Duration,
}

impl Default for StopParams {
    fn default() -> Self {
        Self {
            term_grace: Duration::from_millis(600),
            empty_wait: Duration::from_secs(3),
            output_wait: Duration::from_secs(1),
        }
    }
}

/// Cloneable stop failure (anyhow::Error is not Clone): published by the
/// shared completion task and returned to every waiter — a failure is as
/// bounded and as shared as a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopFailure {
    pub message: String,
}

/// Shared, cloneable stop outcome: `Ok(stats)` or `Err(failure)`.
pub type StopResult = Result<StopStats, StopFailure>;

fn stop_failure_into_error(failure: StopFailure) -> anyhow::Error {
    anyhow::anyhow!("stop failed: {}", failure.message)
}

pub struct SpawnParams<'a> {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: &'a Path,
    pub env: EnvSpec,
    pub rows: u16,
    pub cols: u16,
    pub session_id: String,
    /// Startup fault to inject (tests the state-aware rollback).
    pub fault: Option<FaultStep>,
}

impl<'a> SpawnParams<'a> {
    pub fn new(
        program: &str,
        args: &[&str],
        cwd: &'a Path,
        env: EnvSpec,
        session_id: &str,
    ) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd,
            env,
            rows: 30,
            cols: 120,
            session_id: session_id.to_string(),
            fault: None,
        }
    }
}

/// One entry of the coordinator's ordered commit log. Every entry is
/// appended under the coordinator mutex, so the log order IS the commit
/// order — no interleaving with a switch or another write is possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitEvent {
    /// A generation switch committed (every invalidate/bump).
    Switch { new_generation: u64, seq: u64 },
    /// One successful write syscall committed `bytes` at `generation`.
    Write {
        generation: u64,
        seq: u64,
        bytes: u64,
    },
}

/// Scan of the ordered commit log (the generation evidence). Walking the
/// log in commit order, `current` is the generation set by the latest
/// preceding SwitchCommit; a WriteCommit with `generation < current` is an
/// old-generation commit after a switch (`stale_writes`) and must stay 0.
/// Legal rejections (a generation change observed BEFORE the commit) commit
/// no bytes and never enter the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitScan {
    pub switches: usize,
    pub writes: usize,
    pub stale_writes: usize,
    pub total_bytes: u64,
    pub last_write_generation: Option<u64>,
    /// Generation of the latest WriteCommit that follows the last
    /// SwitchCommit (proof that a post-switch generation still commits).
    pub last_write_after_last_switch: Option<u64>,
}

/// THE linearization point. Generation bumps, stop-intent commits, resize
/// commits, and every actual write syscall acquire this short mutex; nothing
/// ever awaits while holding it. Watch channels are wake-ups only.
struct Coord {
    generation: u64,
    stop_committed: bool,
    /// Monotonic sequence shared by SwitchCommit and WriteCommit entries;
    /// incremented once per appended log entry under this same lock.
    seq: u64,
    /// Ordered commit log: the SwitchCommit/WriteCommit evidence.
    log: Vec<CommitEvent>,
}

struct WriterShared {
    coordinator: Arc<Mutex<Coord>>,
    gen_tx: watch::Sender<u64>,
    stop_tx: watch::Sender<bool>,
    /// Exit signal for the writer task (stop or finalize before joining).
    exit_tx: watch::Sender<bool>,
}

enum WriterCmd {
    Write {
        /// Exact-length payload: normalized at enqueue time so the writer
        /// queue never retains a caller Vec's spare capacity.
        data: Box<[u8]>,
        generation: u64,
        deadline: Duration,
        progress: Arc<AtomicUsize>,
        resp: oneshot::Sender<SendOutcome>,
    },
    Resize {
        rows: u16,
        cols: u16,
        generation: u64,
        resp: oneshot::Sender<Result<(), String>>,
    },
}

pub type EventLog = Arc<Mutex<Vec<(f64, Event)>>>;

fn emit_into(log: &EventLog, t0: Instant, event: Event) {
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    log.lock().unwrap().push((elapsed_ms, event));
}

fn elapsed_ms(t0: Instant) -> u64 {
    t0.elapsed().as_millis() as u64
}

/// Normalize an accepted payload to an exact-length boxed slice: the
/// caller Vec's spare capacity (e.g. an 8 MiB Vec holding a 16-byte
/// message) is released BEFORE the payload enters the accepted queue, so
/// the queue backs exactly `len` payload bytes (allocator metadata aside).
fn normalize_payload(data: Vec<u8>) -> Box<[u8]> {
    let mut data = data;
    data.shrink_to_fit();
    data.into_boxed_slice()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Poll the reader task's output buffer until `pattern` appears or the
/// timeout elapses (false on timeout). Startup-side ready handshake for
/// fault injection: used before a fault whose rollback would close the
/// master side, so the fixture must first prove it reached the state the
/// scenario asserts afterwards.
async fn wait_output_pattern(
    output: &Arc<Mutex<Vec<u8>>>,
    pattern: &[u8],
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let bytes = output.lock().unwrap().clone();
        if find_subslice(&bytes, pattern) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10)).await;
    }
}

pub fn set_winsize(fd: RawFd, rows: u16, cols: u16) -> std::io::Result<()> {
    nix::ioctl_write_ptr_bad!(set_winsize_ioctl, libc::TIOCSWINSZ, libc::winsize);
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { set_winsize_ioctl(fd, &ws) }
        .map(|_| ())
        .map_err(std::io::Error::from)
}

fn raw_write(fd: RawFd, buf: &[u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// A single probe terminal: PTY halves, shared child handle, per-terminal
/// cgroup, coordination state, and its quota slot.
pub struct Terminal {
    pub root_pid: i32,
    pub starttime: u64,
    pub sid: i32,
    slot: u64,
    cg: Mutex<Option<TerminalCgroup>>,
    ctl_fd: OwnedFd,
    shared: Arc<WriterShared>,
    coordinator: Arc<Mutex<Coord>>,
    writer_tx: mpsc::Sender<WriterCmd>,
    writer_handle: Mutex<Option<JoinHandle<()>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    monitor_handle: Mutex<Option<JoinHandle<()>>>,
    output: Arc<Mutex<Vec<u8>>>,
    closed_tx: watch::Sender<bool>,
    close_committed: Arc<AtomicBool>,
    output_forced: AtomicBool,
    exited: Arc<AtomicBool>,
    exit_info: Arc<Mutex<Option<(Option<i32>, Option<i32>)>>>,
    log: EventLog,
    t0: Instant,
    quota: Arc<Quota>,
    quota_released: AtomicBool,
    lifecycle_claimed: AtomicBool,
    stop_claimed: AtomicBool,
    finalized: AtomicBool,
    stop_result_tx: Arc<watch::Sender<Option<StopResult>>>,
}

impl Terminal {
    fn emit(&self, event: Event) {
        emit_into(&self.log, self.t0, event);
    }

    pub fn events(&self) -> Vec<(f64, Event)> {
        self.log.lock().unwrap().clone()
    }

    pub fn count_events(&self, pred: impl Fn(&Event) -> bool) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| pred(e))
            .count()
    }

    pub async fn wait_event(
        &self,
        pred: impl Fn(&Event) -> bool,
        timeout: Duration,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let found = self
                .log
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(_, event)| pred(event))
                .map(|(_, event)| event.clone());
            if let Some(event) = found {
                return Some(event);
            }
            if Instant::now() >= deadline {
                return None;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn wait_output_contains(&self, pattern: &str, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            let bytes = self.output.lock().unwrap().clone();
            if find_subslice(&bytes, pattern.as_bytes()) {
                return Some(bytes);
            }
            if Instant::now() >= deadline {
                return None;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn output_tail(&self, n: usize) -> Vec<u8> {
        let output = self.output.lock().unwrap();
        output[output.len().saturating_sub(n)..].to_vec()
    }

    pub fn is_output_closed(&self) -> bool {
        *self.closed_tx.borrow()
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// (exit_code, signal) once the root was reaped; exactly one is ever set.
    pub fn exit_info(&self) -> Option<(Option<i32>, Option<i32>)> {
        self.exit_info.lock().unwrap().clone()
    }

    pub fn quota_state(&self) -> SlotState {
        self.quota.state(self.slot).unwrap_or(SlotState::Released)
    }

    /// Scan of the coordinator's ordered commit log — the generation
    /// evidence. Walking SwitchCommit/WriteCommit entries in commit order,
    /// `stale_writes` counts every WriteCommit whose generation is below the
    /// current generation (set by the latest preceding SwitchCommit) and
    /// must stay 0.
    pub fn commit_scan(&self) -> CommitScan {
        let coord = self.coordinator.lock().unwrap();
        let mut scan = CommitScan {
            switches: 0,
            writes: 0,
            stale_writes: 0,
            total_bytes: 0,
            last_write_generation: None,
            last_write_after_last_switch: None,
        };
        // The current generation before any logged switch is the initial one.
        let mut current = 1u64;
        let mut saw_switch = false;
        let mut write_since_switch: Option<u64> = None;
        for event in &coord.log {
            match event {
                CommitEvent::Switch { new_generation, .. } => {
                    current = *new_generation;
                    scan.switches += 1;
                    saw_switch = true;
                    write_since_switch = None;
                }
                CommitEvent::Write {
                    generation, bytes, ..
                } => {
                    if *generation < current {
                        scan.stale_writes += 1;
                    }
                    scan.writes += 1;
                    scan.total_bytes += bytes;
                    scan.last_write_generation = Some(*generation);
                    write_since_switch = Some(*generation);
                }
            }
        }
        scan.last_write_after_last_switch = if saw_switch {
            write_since_switch
        } else {
            scan.last_write_generation
        };
        scan
    }

    /// Total bytes committed under the coordinator lock (the sum of the
    /// WriteCommit log entries).
    pub fn total_committed(&self) -> u64 {
        self.commit_scan().total_bytes
    }

    /// Foreground process group of the pty (0 = none; never a signal target).
    pub fn tcgetpgrp_now(&self) -> Option<i32> {
        nix::unistd::tcgetpgrp(self.ctl_fd.as_fd())
            .ok()
            .map(|p| p.as_raw())
    }

    /// Raw + noecho so control bytes reach the fixture untouched.
    pub fn set_raw(&self) -> Result<(), String> {
        use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
        let mut termios = tcgetattr(self.ctl_fd.as_fd()).map_err(|e| format!("tcgetattr: {e}"))?;
        cfmakeraw(&mut termios);
        tcsetattr(self.ctl_fd.as_fd(), SetArg::TCSANOW, &termios)
            .map_err(|e| format!("tcsetattr: {e}"))
    }

    pub fn generation_now(&self) -> u64 {
        self.coordinator.lock().unwrap().generation
    }

    /// Bump the control-lease generation. The commit (and its SwitchCommit
    /// log entry) happens under the coordinator lock, so it is totally
    /// ordered against every write syscall.
    pub fn invalidate_generation(&self) -> u64 {
        let next = {
            let mut coord = self.coordinator.lock().unwrap();
            coord.generation += 1;
            coord.seq += 1;
            let (new_generation, seq) = (coord.generation, coord.seq);
            coord.log.push(CommitEvent::Switch {
                new_generation,
                seq,
            });
            new_generation
        };
        let _ = self.shared.gen_tx.send(next);
        next
    }

    /// A shareable handle for concurrent generation bumping (race scenarios).
    /// The bump commit happens under the same coordinator lock as every
    /// write syscall, so a bump's commit point is totally ordered against
    /// every byte.
    pub fn generation_handle(&self) -> GenerationHandle {
        GenerationHandle {
            coordinator: self.coordinator.clone(),
            gen_tx: self.shared.gen_tx.clone(),
        }
    }

    /// The terminal's cgroup path (from the Spawned event).
    pub fn cgroup_path(&self) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|(_, e)| match e {
                Event::Spawned { cgroup, .. } => Some(cgroup.clone()),
                _ => None,
            })
    }

    /// Whether the terminal cgroup currently holds live processes.
    pub fn cgroup_populated(&self) -> bool {
        self.cg
            .lock()
            .unwrap()
            .as_ref()
            .map(|cg| cgroup::cg_populated(&cg.path))
            .unwrap_or(false)
    }

    /// Queue a bounded write; returns the progress counter (bytes actually
    /// committed to the PTY master so far) and the outcome receiver.
    pub fn send_tracked(
        &self,
        data: Vec<u8>,
        generation: u64,
    ) -> (Arc<AtomicUsize>, oneshot::Receiver<SendOutcome>) {
        self.send_tracked_deadline(data, generation, DEFAULT_WRITE_DEADLINE)
    }

    pub fn send_tracked_deadline(
        &self,
        data: Vec<u8>,
        generation: u64,
        deadline: Duration,
    ) -> (Arc<AtomicUsize>, oneshot::Receiver<SendOutcome>) {
        let (tx, rx) = oneshot::channel();
        let progress = Arc::new(AtomicUsize::new(0));
        // Typed size validation BEFORE enqueueing: an oversize message never
        // enters the queue, so accepted queued+inflight bytes stay bounded
        // by (WRITE_QUEUE_CAPACITY + 1) * MAX_SEND_BYTES.
        if data.len() > MAX_SEND_BYTES {
            self.emit(Event::SendRejectedOversize);
            let _ = tx.send(SendOutcome::RejectedOversize);
            return (progress, rx);
        }
        {
            let coord = self.coordinator.lock().unwrap();
            if coord.stop_committed {
                drop(coord);
                self.emit(Event::SendRejectedAfterStop);
                let _ = tx.send(SendOutcome::RejectedAfterStop);
                return (progress, rx);
            }
            if coord.generation != generation {
                drop(coord);
                self.emit(Event::SendRejectedStaleGeneration);
                let _ = tx.send(SendOutcome::RejectedStaleGeneration);
                return (progress, rx);
            }
        }
        match self.writer_tx.try_send(WriterCmd::Write {
            // Exact-length normalization at the enqueue boundary: the
            // accepted queue only backs `len` payload bytes per message —
            // never the caller Vec's spare capacity (allocator metadata
            // aside), so the 768 KiB accepted bound is a payload-byte bound.
            data: normalize_payload(data),
            generation,
            deadline,
            progress: progress.clone(),
            resp: tx,
        }) {
            Ok(()) => (progress, rx),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.emit(Event::SendRejectedQueueFull);
                let (tx2, rx2) = oneshot::channel();
                let _ = tx2.send(SendOutcome::RejectedQueueFull);
                (progress, rx2)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let (tx2, rx2) = oneshot::channel();
                let _ = tx2.send(SendOutcome::Aborted {
                    written: 0,
                    reason: AbortReason::WriteFailed("writer gone".into()),
                });
                (progress, rx2)
            }
        }
    }

    pub async fn send(&self, data: Vec<u8>, generation: u64) -> SendOutcome {
        let (_progress, rx) = self.send_tracked(data, generation);
        match rx.await {
            Ok(outcome) => outcome,
            Err(_) => SendOutcome::Aborted {
                written: 0,
                reason: AbortReason::WriteFailed("outcome channel dropped".into()),
            },
        }
    }

    /// Resize, ordered against sends by the single writer task, with the
    /// generation checked at the actual commit point.
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let generation = self.generation_now();
        let (tx, rx) = oneshot::channel();
        self.writer_tx
            .try_send(WriterCmd::Resize {
                rows,
                cols,
                generation,
                resp: tx,
            })
            .map_err(|_| "writer queue unavailable".to_string())?;
        rx.await.map_err(|_| "writer dropped".to_string())?
    }

    /// Stop barrier. Idempotent: concurrent calls share one intent, one
    /// completion, and one quota release. The committed stop sequence runs
    /// in a DETACHED completion task — cancelling a caller can never undo a
    /// committed stop — and EVERY leader exit (success or failure) publishes
    /// the shared cloneable result, so every waiter returns bounded with
    /// exactly that outcome. A stop after a completed stop (or after a
    /// natural finalize) returns the existing state without new events.
    pub async fn stop(self: &Arc<Self>, params: StopParams) -> Result<StopStats> {
        // A completed stop's shared result first (idempotent stops must
        // return exactly what the first stop returned — success or failure);
        // then the finalized fast path for terminals that ended naturally
        // without ever being stopped.
        if let Some(result) = self.stop_result_tx.borrow().clone() {
            return result.map_err(stop_failure_into_error);
        }
        if self.finalized.load(Ordering::SeqCst) {
            return Ok(self.existing_state_stats());
        }
        // Existing-state fast path: process and output already ended AND the
        // terminal cgroup is already empty — nothing left to stop. Finish the
        // cleanup (join tasks, remove cgroup, release quota) and report the
        // current state without a new stop sequence (protocol: "仅当进程与
        // 输出均已结束时，Stop 直接返回已有状态").
        if self.is_exited() && self.is_output_closed() {
            let cg_path = self.cg.lock().unwrap().as_ref().map(|cg| cg.path.clone());
            let empty = cg_path
                .as_ref()
                .map(|p| !cgroup::cg_populated(p))
                .unwrap_or(true);
            if empty {
                let _ = self.shared.exit_tx.send(true);
                self.join_management_tasks().await;
                if let Some(cg) = self.cg.lock().unwrap().take() {
                    cg.remove()
                        .map_err(|e| anyhow::anyhow!("remove terminal cgroup: {e}"))?;
                }
                self.finish_lifecycle();
                return Ok(self.existing_state_stats());
            }
        }
        if !self.stop_claimed.swap(true, Ordering::SeqCst) {
            // Leader: run the committed stop sequence in a detached
            // completion task so caller cancellation cannot undo it. The
            // wrapper publishes the result on every exit path (success or
            // failure), so waiters can never wait forever.
            let terminal = Arc::clone(self);
            tokio::spawn(async move {
                let result = terminal.run_stop(params).await;
                let shared = result.map_err(|e| StopFailure {
                    message: format!("{e:#}"),
                });
                terminal.stop_result_tx.send_replace(Some(shared));
            });
        }
        // Leader and waiters alike observe the shared published result.
        let mut rx = self.stop_result_tx.subscribe();
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result.map_err(stop_failure_into_error);
            }
            if rx.changed().await.is_err() {
                anyhow::bail!("stop completion ended without publishing a result");
            }
        }
    }

    fn existing_state_stats(&self) -> StopStats {
        StopStats {
            forced: false,
            term_phase_ms: 0,
            kill_phase_ms: 0,
            total_ms: 0,
            term_rounds: 0,
            signalled: 0,
            output_forced_close: self.output_forced.load(Ordering::SeqCst),
            existing_state: true,
        }
    }

    async fn run_stop(&self, params: StopParams) -> Result<StopStats> {
        let t0 = Instant::now();

        // 1. Commit the stop intent under the coordinator lock: in-flight
        //    writes abort at their next critical section; new sends and
        //    resizes are rejected at their commit points.
        {
            let mut coord = self.coordinator.lock().unwrap();
            coord.stop_committed = true;
        }
        let _ = self.shared.stop_tx.send(true);
        let _ = self.shared.exit_tx.send(true);
        self.emit(Event::StopIntentCommitted);

        let cg_path: Option<PathBuf> = self.cg.lock().unwrap().as_ref().map(|cg| cg.path.clone());

        // 2. Gentle phase: identity-safe SIGTERM to current cgroup members,
        //    rescanning so TERM-time forks are also signalled, until the
        //    cgroup empties or the grace expires.
        let mut term_rounds = 0usize;
        let mut signalled = 0usize;
        if let Some(cg) = &cg_path {
            let term_deadline = Instant::now() + params.term_grace;
            while cgroup::cg_populated(cg) && Instant::now() < term_deadline {
                signalled += cgroup::term_signal_members(cg);
                term_rounds += 1;
                let scan_deadline =
                    (Instant::now() + Duration::from_millis(100)).min(term_deadline);
                while Instant::now() < scan_deadline && cgroup::cg_populated(cg) {
                    sleep(Duration::from_millis(20)).await;
                }
            }
        }
        let graceful = cg_path
            .as_ref()
            .map(|c| !cgroup::cg_populated(c))
            .unwrap_or(true);
        let term_phase_ms = elapsed_ms(t0);

        // 3. Forced phase: cgroup.kill is the fixed point — SIGKILL to the
        //    whole subtree, safe against concurrent forks and new pgroups.
        let mut forced = false;
        if let Some(cg) = &cg_path {
            if !graceful {
                cgroup::cg_kill(cg).map_err(|e| anyhow::anyhow!("write cgroup.kill: {e}"))?;
                forced = true;
                anyhow::ensure!(
                    cgroup::cg_wait_empty(cg, params.empty_wait).await,
                    "terminal cgroup not empty after cgroup.kill"
                );
            }
        }
        let kill_phase_ms = elapsed_ms(t0);

        // 4. Root must be reaped (the monitor's try_wait loop reaps it).
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        while !self.exited.load(Ordering::SeqCst) {
            anyhow::ensure!(
                Instant::now() < reap_deadline,
                "root {} not reaped after stop",
                self.root_pid
            );
            sleep(Duration::from_millis(10)).await;
        }

        // 5. Output must end. All pts holders are dead now, so the master
        //    read returns EIO; if something escaped the cgroup AND still
        //    holds the pts, force-close after the bounded wait.
        let output_deadline = Instant::now() + params.output_wait;
        let mut closed_rx = self.closed_tx.subscribe();
        while !*closed_rx.borrow_and_update() {
            if Instant::now() >= output_deadline {
                self.force_close_output();
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        anyhow::ensure!(*self.closed_tx.borrow(), "output not closed after stop");

        // 6. Management tasks must be joined before the quota is released.
        self.join_management_tasks().await;

        // 7. Remove the (now empty) terminal cgroup.
        if let Some(cg) = self.cg.lock().unwrap().take() {
            cg.remove()
                .map_err(|e| anyhow::anyhow!("remove terminal cgroup: {e}"))?;
        }

        let total_ms = elapsed_ms(t0);
        let output_forced_close = self.output_forced.load(Ordering::SeqCst);

        // 8. Only now — cgroup empty, root reaped, tasks joined, cgroup
        //    removed — is the quota slot released.
        self.finish_lifecycle();
        let stats = StopStats {
            forced,
            term_phase_ms,
            kill_phase_ms,
            total_ms,
            term_rounds,
            signalled,
            output_forced_close,
            existing_state: false,
        };
        self.emit(Event::StopCompleted {
            forced: stats.forced,
            term_phase_ms: stats.term_phase_ms,
            kill_phase_ms: stats.kill_phase_ms,
            total_ms: stats.total_ms,
            term_rounds: stats.term_rounds,
            signalled: stats.signalled,
        });
        // The detached completion-task wrapper publishes the result (success
        // or failure) to the shared watch channel — never here.
        Ok(stats)
    }

    /// Natural-completion path: root exited on its own, output closed on its
    /// own, and the cgroup must already be empty (a terminal with live
    /// members needs stop, not finalize).
    pub async fn finalize(&self) -> Result<()> {
        anyhow::ensure!(
            self.is_exited(),
            "finalize requires the root to have exited"
        );
        anyhow::ensure!(
            self.is_output_closed(),
            "finalize requires the output to be closed"
        );
        let cg_path = self.cg.lock().unwrap().as_ref().map(|cg| cg.path.clone());
        if let Some(cg) = &cg_path {
            anyhow::ensure!(
                cgroup::cg_wait_empty(cg, Duration::from_secs(5)).await,
                "terminal cgroup still has live members at finalize; use stop"
            );
        }
        let _ = self.shared.exit_tx.send(true);
        self.join_management_tasks().await;
        if let Some(cg) = self.cg.lock().unwrap().take() {
            cg.remove()
                .map_err(|e| anyhow::anyhow!("remove terminal cgroup: {e}"))?;
        }
        self.finish_lifecycle();
        Ok(())
    }

    async fn join_management_tasks(&self) {
        // Take each handle inside a short critical section; a std MutexGuard
        // must never be held across an await (latent deadlock + non-Send).
        let reader = self.reader_handle.lock().unwrap().take();
        if let Some(handle) = reader {
            handle.abort();
            let _ = handle.await;
        }
        let writer = self.writer_handle.lock().unwrap().take();
        if let Some(handle) = writer {
            let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
        }
        let monitor = self.monitor_handle.lock().unwrap().take();
        if let Some(handle) = monitor {
            let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
        }
    }

    fn finish_lifecycle(&self) {
        if !self.lifecycle_claimed.swap(true, Ordering::SeqCst) {
            let _ = self.quota.begin_cleaning(self.slot);
            self.release_quota();
            self.finalized.store(true, Ordering::SeqCst);
        }
    }

    fn release_quota(&self) {
        if !self.quota_released.swap(true, Ordering::SeqCst) {
            self.quota
                .release(self.slot)
                .expect("quota slot released exactly once");
            self.emit(Event::QuotaReleased);
        }
    }

    /// Force-close the output side: abort a blocked master read and commit a
    /// Forced end through the same one-shot as the natural path, so exactly
    /// one OutputClosed event exists whichever side wins.
    fn force_close_output(&self) {
        if self.close_committed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.output_forced.store(true, Ordering::SeqCst);
        if let Some(handle) = self.reader_handle.lock().unwrap().take() {
            handle.abort();
        }
        self.closed_tx.send_replace(true);
        self.emit(Event::OutputClosed {
            end: OutputEnd::Forced,
        });
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Safety net if a scenario forgets the lifecycle: wake everything.
        // The probe driver always stops or finalizes explicitly.
        {
            let mut coord = self.coordinator.lock().unwrap();
            coord.stop_committed = true;
        }
        let _ = self.shared.stop_tx.send(true);
        let _ = self.shared.exit_tx.send(true);
    }
}

/// Shareable generation bumping for the race scenario. Every bump commits
/// under the coordinator mutex — the same lock every write syscall takes.
pub struct GenerationHandle {
    coordinator: Arc<Mutex<Coord>>,
    gen_tx: watch::Sender<u64>,
}

impl GenerationHandle {
    pub fn bump(&self) -> u64 {
        let next = {
            let mut coord = self.coordinator.lock().unwrap();
            coord.generation += 1;
            coord.seq += 1;
            let (new_generation, seq) = (coord.generation, coord.seq);
            coord.log.push(CommitEvent::Switch {
                new_generation,
                seq,
            });
            new_generation
        };
        let _ = self.gen_tx.send(next);
        next
    }
}

pub struct Terminals {
    quota: Arc<Quota>,
    cgroup_root: ProbeCgroupRoot,
    shutdown_tx: watch::Sender<bool>,
    instance: String,
    spawned_log: Mutex<File>,
}

impl Terminals {
    pub fn new(
        session_limit: usize,
        global_limit: usize,
        workdir: &Path,
        instance: &str,
    ) -> anyhow::Result<Self> {
        let tag = workdir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("workdir must have a file name"))?;
        let cgroup_root = ProbeCgroupRoot::open_or_create(tag)?;
        // Record the root path so the run.sh cleanup sweep can find it.
        std::fs::write(
            workdir.join("cgroup-root.json"),
            serde_json::json!({ "path": cgroup_root.path }).to_string(),
        )
        .map_err(|e| anyhow::anyhow!("write cgroup root marker: {e}"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(workdir.join("spawned.log"))
            .map_err(|e| anyhow::anyhow!("open spawned log: {e}"))?;
        Ok(Self {
            quota: Arc::new(Quota::new(session_limit, global_limit)),
            cgroup_root,
            shutdown_tx: watch::channel(false).0,
            instance: instance.to_string(),
            spawned_log: Mutex::new(file),
        })
    }

    pub fn quota(&self) -> &Quota {
        &self.quota
    }

    pub fn terminal_cgroup_names(&self) -> Vec<String> {
        self.cgroup_root.terminal_names()
    }

    /// Service shutdown: every pending write aborts with ServiceShutdown.
    pub fn signal_shutdown(&self) {
        self.shutdown_tx.send_replace(true);
    }

    fn append_spawned(&self, pid: i32, starttime: u64) {
        let mut file = self.spawned_log.lock().unwrap();
        let _ = writeln!(file, "{pid} {starttime}");
        let _ = file.flush();
    }

    pub async fn spawn(&self, params: SpawnParams<'_>) -> Result<Terminal, SpawnError> {
        // Missing snapshot is rejected before quota, cgroup, PTY, or process.
        if matches!(params.env, EnvSpec::Missing) {
            return Err(SpawnError::EnvMissing);
        }
        let slot = self
            .quota
            .reserve(&params.session_id)
            .map_err(SpawnError::Quota)?;
        self.spawn_inner(&params, slot).await
    }

    async fn spawn_inner(
        &self,
        params: &SpawnParams<'_>,
        slot: u64,
    ) -> Result<Terminal, SpawnError> {
        let fault = params.fault;
        let cg = match self
            .cgroup_root
            .create_terminal(&format!("{}-t{slot}", self.instance))
        {
            Ok(cg) => cg,
            Err(e) => {
                // No resources beyond the reservation exist yet.
                let _ = self.quota.start_failed(slot);
                return Err(SpawnError::StartFailed(format!(
                    "create terminal cgroup: {e}"
                )));
            }
        };
        let cg_path_string = cg.path.to_string_lossy().into_owned();

        // Everything below this point rolls back through `rollback` on error.
        let mut rollback = Rollback {
            quota: &self.quota,
            slot,
            cg: Some(cg),
            child: None,
            tasks: Vec::new(),
            shared: None,
            no_kill: false,
            reap_fault: None,
        };

        // The pre-opened cgroup.procs fd for the pre_exec join; taken from
        // the guarded cgroup so a failure here also rolls back.
        let join_fd = {
            let guard = rollback.cg.as_mut().expect("cgroup in rollback");
            match guard.take_join_fd() {
                Some(fd) => fd,
                None => {
                    let error = SpawnError::StartFailed("cgroup join fd already taken".into());
                    return Err(rollback.run(error).await);
                }
            }
        };

        if fault == Some(FaultStep::BeforePty) {
            let error = SpawnError::InjectedFault(FaultStep::BeforePty.name());
            return Err(rollback.run(error).await);
        }

        let (pty, pts) = match pty_process::open() {
            Ok(v) => v,
            Err(e) => {
                let error = SpawnError::StartFailed(format!("open pty: {e}"));
                return Err(rollback.run(error).await);
            }
        };
        if let Err(e) = pty.resize(pty_process::Size::new(params.rows, params.cols)) {
            let error = SpawnError::StartFailed(format!("resize pty: {e}"));
            return Err(rollback.run(error).await);
        }
        let ctl_fd: OwnedFd = match pty.as_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                let error = SpawnError::StartFailed(format!("clone master fd: {e}"));
                return Err(rollback.run(error).await);
            }
        };
        // The write side: another dup of the master (same open file
        // description; pty-process already set O_NONBLOCK) wrapped in AsyncFd.
        let writer_fd: OwnedFd = match pty.as_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                let error = SpawnError::StartFailed(format!("clone write fd: {e}"));
                return Err(rollback.run(error).await);
            }
        };

        if fault == Some(FaultStep::AfterPtyOpen) {
            let error = SpawnError::InjectedFault(FaultStep::AfterPtyOpen.name());
            return Err(rollback.run(error).await);
        }

        let env_pairs: Vec<(String, String)> = match &params.env {
            EnvSpec::Snapshot(pairs) => pairs.clone(),
            EnvSpec::Empty => Vec::new(),
            EnvSpec::Missing => unreachable!("checked before reservation"),
        };

        // Join the terminal cgroup before exec: pre-opened cgroup.procs fd,
        // async-signal-safe write of "0" (= self) in the pre_exec closure.
        // pty-process composes this after its setsid + TIOCSCTTY hook.
        let mut command = pty_process::Command::new(&params.program)
            .args(&params.args)
            .current_dir(params.cwd)
            .env_clear();
        for (key, value) in &env_pairs {
            command = command.env(key, value);
        }
        // SAFETY: the closure only calls write(2), which is async-signal-safe.
        command = unsafe {
            command.pre_exec(move || {
                let buf = b"0";
                let n = libc::write(join_fd.as_raw_fd(), buf.as_ptr().cast(), 1);
                if n == 1 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            })
        };
        let child = match command.spawn(pts) {
            Ok(child) => child,
            Err(e) => {
                let error = SpawnError::StartFailed(format!("spawn {}: {e}", params.program));
                return Err(rollback.run(error).await);
            }
        };
        let root_pid = match child.id() {
            Some(id) => id as i32,
            None => {
                let error = SpawnError::StartFailed("root exited before pid capture".into());
                return Err(rollback.run(error).await);
            }
        };

        // Coordination + shared state (needed for the AfterTaskStart rollback).
        let t0 = Instant::now();
        let log: EventLog = Arc::new(Mutex::new(Vec::new()));
        let coordinator = Arc::new(Mutex::new(Coord {
            generation: 1,
            stop_committed: false,
            seq: 0,
            log: Vec::new(),
        }));
        let (gen_tx, _) = watch::channel(1u64);
        let (stop_tx, _) = watch::channel(false);
        let (exit_tx, _) = watch::channel(false);
        let shared = Arc::new(WriterShared {
            coordinator: coordinator.clone(),
            gen_tx,
            stop_tx: stop_tx.clone(),
            exit_tx: exit_tx.clone(),
        });
        rollback.shared = Some(shared.clone());

        let (reader, _dropped_write_half) = pty.into_split();
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (closed_tx, _output_closed) = watch::channel(false);
        let close_committed = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let exit_info = Arc::new(Mutex::new(None));
        let child_shared: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
        rollback.child = Some(child_shared.clone());

        // Reader task (pty-process AsyncRead): drains the master; the final
        // read after the last slave closes is EIO on Linux, never Ok(0).
        // OutputClosed commits through the one-shot atomic exactly once.
        let reader_task_coord = coordinator.clone();
        let reader_handle = {
            let log = log.clone();
            let t0 = t0;
            let output = output.clone();
            let closed_tx = closed_tx.clone();
            let close_committed = close_committed.clone();
            let coordinator = reader_task_coord;
            let handle = tokio::spawn(async move {
                let mut reader = reader;
                let mut buf = vec![0u8; 8192];
                let end = loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => {
                            break classify_close(&coordinator);
                        }
                        Ok(n) => output.lock().unwrap().extend_from_slice(&buf[..n]),
                        Err(e) => {
                            if e.raw_os_error() == Some(5) {
                                break classify_close(&coordinator);
                            }
                            break OutputEnd::ReadError(format!("{e}"));
                        }
                    }
                };
                if !close_committed.swap(true, Ordering::SeqCst) {
                    closed_tx.send_replace(true);
                    emit_into(&log, t0, Event::OutputClosed { end });
                }
            });
            handle
        };
        // The handle enters the rollback guard the MOMENT the task exists, so
        // every failure path below reclaims it (the success path takes it
        // back out at the end).
        rollback.tasks.push(reader_handle);

        // Monitor task: reaps the root; try_wait errors emit MonitorFault and
        // NEVER fabricate a ProcessExited.
        let monitor_handle = {
            let log = log.clone();
            let t0 = t0;
            let exited = exited.clone();
            let exit_info = exit_info.clone();
            let child = child_shared.clone();
            let monitor_poll_fault = match fault {
                Some(FaultStep::MonitorPoll(n)) => Some(n),
                _ => None,
            };
            let handle = tokio::spawn(async move {
                let mut polls: usize = 0;
                loop {
                    polls += 1;
                    if monitor_poll_fault == Some(polls) {
                        emit_into(
                            &log,
                            t0,
                            Event::MonitorFault {
                                message: "injected try_wait error".into(),
                            },
                        );
                    }
                    let result = {
                        let mut guard = child.lock().unwrap();
                        match guard.as_mut() {
                            Some(child) => child.try_wait(),
                            None => return,
                        }
                    };
                    match result {
                        Ok(Some(status)) => {
                            *exit_info.lock().unwrap() = Some((status.code(), status.signal()));
                            exited.store(true, Ordering::SeqCst);
                            emit_into(
                                &log,
                                t0,
                                Event::ProcessExited {
                                    code: status.code(),
                                    signal: status.signal(),
                                },
                            );
                            return;
                        }
                        Ok(None) => sleep(Duration::from_millis(15)).await,
                        Err(e) => {
                            emit_into(
                                &log,
                                t0,
                                Event::MonitorFault {
                                    message: format!("try_wait: {e}"),
                                },
                            );
                            sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            });
            handle
        };
        // Same immediate guard entry for the monitor task.
        rollback.tasks.push(monitor_handle);

        if fault == Some(FaultStep::AfterSpawn) {
            let error = SpawnError::InjectedFault(FaultStep::AfterSpawn.name());
            return Err(rollback.run(error).await);
        }

        let stat = match proc::read_stat(root_pid) {
            Some(stat) => stat,
            None => {
                let error = SpawnError::StartFailed("root /proc stat vanished".into());
                return Err(rollback.run(error).await);
            }
        };
        if fault == Some(FaultStep::AfterProcStat) {
            let error = SpawnError::InjectedFault(FaultStep::AfterProcStat.name());
            return Err(rollback.run(error).await);
        }
        self.append_spawned(root_pid, stat.starttime);
        if let Err(e) = self.quota.internalize(slot) {
            return Err(rollback.run(SpawnError::Quota(e)).await);
        }

        // Deterministic ready handshake for RollbackNoKill: the injected
        // rollback drops every master-side fd, which SIGHUPs the terminal's
        // foreground process group. Until the fixture has installed its
        // SIGHUP-ignore handlers it may die right there, the terminal cgroup
        // would already be empty, and the scenario's "stays populated"
        // proof would be a race. The park fixture prints PARKING only AFTER
        // installing the handlers, so waiting for that line is a race-free
        // synchronization before the fault executes. Hard 5s bound: on
        // timeout the fault is NOT injected — a normal killing rollback
        // fails the spawn so the probe hard-fails deterministically.
        if fault == Some(FaultStep::RollbackNoKill)
            && !wait_output_pattern(&output, b"PARKING", Duration::from_secs(5)).await
        {
            let error = SpawnError::StartFailed(
                "RollbackNoKill fixture never announced PARKING within 5s".into(),
            );
            return Err(rollback.run(error).await);
        }

        if let Some(
            step @ (FaultStep::AfterTaskStart
            | FaultStep::RollbackNoKill
            | FaultStep::RollbackReapError
            | FaultStep::RollbackReapTimeout),
        ) = fault
        {
            rollback.no_kill = fault == Some(FaultStep::RollbackNoKill);
            rollback.reap_fault = match fault {
                Some(FaultStep::RollbackReapError) => Some(ReapFault::TryWaitError),
                Some(FaultStep::RollbackReapTimeout) => Some(ReapFault::NeverReaped),
                _ => None,
            };
            return Err(rollback.run(SpawnError::InjectedFault(step.name())).await);
        }

        // Writer task: bounded queue; the write loop keeps readiness waits,
        // the deadline and service-shutdown OUTSIDE the coordinator lock.
        let (writer_tx, writer_rx) = mpsc::channel::<WriterCmd>(WRITE_QUEUE_CAPACITY);
        let writer_async_fd = match AsyncFd::new(writer_fd) {
            Ok(fd) => fd,
            Err(e) => {
                let error = SpawnError::StartFailed(format!("AsyncFd writer: {e}"));
                return Err(rollback.run(error).await);
            }
        };
        let writer_shared = shared.clone();
        let writer_shutdown = self.shutdown_tx.subscribe();
        let writer_log = log.clone();
        let writer_t0 = t0;
        let writer_handle = tokio::spawn(async move {
            let fd = writer_async_fd;
            let mut rx = writer_rx;
            let mut shutdown_rx = writer_shutdown;
            let mut stop_rx = writer_shared.stop_tx.subscribe();
            let mut exit_rx = writer_shared.exit_tx.subscribe();
            loop {
                let cmd = tokio::select! {
                    biased;
                    _ = stop_rx.changed(), if *stop_rx.borrow() => {
                        drain_with_rejections(&mut rx);
                        break;
                    }
                    _ = exit_rx.changed(), if *exit_rx.borrow() => {
                        drain_with_rejections(&mut rx);
                        break;
                    }
                    _ = shutdown_rx.changed(), if *shutdown_rx.borrow() => {
                        // Service shutdown: the in-flight write aborts through
                        // its own select; queued ones are rejected here.
                        drain_with_rejections(&mut rx);
                        break;
                    }
                    cmd = rx.recv() => match cmd {
                        Some(cmd) => cmd,
                        None => break,
                    },
                };
                match cmd {
                    WriterCmd::Resize {
                        rows,
                        cols,
                        generation,
                        resp,
                    } => {
                        // Commit point: generation checked under the
                        // coordinator lock together with the ioctl.
                        let result = {
                            let coord = writer_shared.coordinator.lock().unwrap();
                            if coord.stop_committed {
                                Err("terminal stopped".to_string())
                            } else if coord.generation != generation {
                                Err("stale generation".to_string())
                            } else {
                                set_winsize(fd.as_raw_fd(), rows, cols).map_err(|e| e.to_string())
                            }
                        };
                        match &result {
                            Ok(()) => emit_into(
                                &writer_log,
                                writer_t0,
                                Event::ResizeApplied { rows, cols },
                            ),
                            Err(reason) => emit_into(
                                &writer_log,
                                writer_t0,
                                Event::ResizeRejected {
                                    reason: reason.clone(),
                                },
                            ),
                        }
                        let _ = resp.send(result);
                    }
                    WriterCmd::Write {
                        data,
                        generation,
                        deadline,
                        progress,
                        resp,
                    } => {
                        let mut write_shutdown = shutdown_rx.clone();
                        let outcome = write_nonblocking(
                            &fd,
                            &data,
                            generation,
                            deadline,
                            &progress,
                            &writer_shared,
                            &mut write_shutdown,
                        )
                        .await;
                        emit_into(
                            &writer_log,
                            writer_t0,
                            match &outcome {
                                SendOutcome::Complete { written } => {
                                    Event::SendComplete { written: *written }
                                }
                                SendOutcome::Aborted { written, reason } => Event::SendAborted {
                                    written: *written,
                                    reason: reason.clone(),
                                },
                                SendOutcome::RejectedAfterStop => Event::SendRejectedAfterStop,
                                SendOutcome::RejectedStaleGeneration => {
                                    Event::SendRejectedStaleGeneration
                                }
                                SendOutcome::RejectedQueueFull => Event::SendRejectedQueueFull,
                                SendOutcome::RejectedOversize => Event::SendRejectedOversize,
                            },
                        );
                        let _ = resp.send(outcome);
                    }
                }
            }
        });
        rollback.tasks.push(writer_handle);

        emit_into(
            &log,
            t0,
            Event::Spawned {
                pid: root_pid,
                cgroup: cg_path_string,
            },
        );
        let cg = rollback
            .cg
            .take()
            .expect("terminal cgroup present on the success path");
        // Success: reclaim the management task handles from the rollback
        // guard (pushed in creation order: reader, monitor, writer).
        let writer_handle = rollback
            .tasks
            .pop()
            .expect("writer handle in rollback guard");
        let monitor_handle = rollback
            .tasks
            .pop()
            .expect("monitor handle in rollback guard");
        let reader_handle = rollback
            .tasks
            .pop()
            .expect("reader handle in rollback guard");
        Ok(Terminal {
            root_pid,
            starttime: stat.starttime,
            sid: stat.session,
            slot,
            cg: Mutex::new(Some(cg)),
            ctl_fd,
            shared,
            coordinator,
            writer_tx,
            writer_handle: Mutex::new(Some(writer_handle)),
            reader_handle: Mutex::new(Some(reader_handle)),
            monitor_handle: Mutex::new(Some(monitor_handle)),
            output,
            closed_tx,
            close_committed,
            output_forced: AtomicBool::new(false),
            exited,
            exit_info,
            log,
            t0,
            quota: self.quota.clone(),
            quota_released: AtomicBool::new(false),
            lifecycle_claimed: AtomicBool::new(false),
            stop_claimed: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
            stop_result_tx: Arc::new(watch::channel(Option::<StopResult>::None).0),
        })
    }
}

fn classify_close(coordinator: &Arc<Mutex<Coord>>) -> OutputEnd {
    let coord = coordinator.lock().unwrap();
    if coord.stop_committed {
        OutputEnd::Forced
    } else {
        OutputEnd::Eof
    }
}

fn drain_with_rejections(rx: &mut mpsc::Receiver<WriterCmd>) {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            WriterCmd::Write { resp, .. } => {
                let _ = resp.send(SendOutcome::RejectedAfterStop);
            }
            WriterCmd::Resize { resp, .. } => {
                let _ = resp.send(Err("terminal stopped".to_string()));
            }
        }
    }
}

/// Startup-fault injection for the rollback's root-reap verification: both
/// failure modes must land in the cleanup failures (slot stays in Cleaning,
/// never released).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReapFault {
    /// The reap loop observes a synthetic try_wait error.
    TryWaitError,
    /// The root is never observed reaped before the rollback deadline.
    NeverReaped,
}

/// State-aware startup rollback: commit stop first (so any started writer
/// aborts instead of writing), kill via the terminal cgroup (identity-safe by
/// construction), reap the child, join any started tasks, drop fds, remove
/// the cgroup — and release the reservation ONLY when the cleanup is
/// verifiably complete (cgroup confirmed empty, root reaped without a
/// try_wait error and before the deadline, every task joined, cgroup
/// removed). Otherwise the slot stays in Cleaning and the caller gets
/// CleanupFailed, so the probe hard-fails instead of silently freeing a slot
/// with leaked processes.
struct Rollback<'a> {
    quota: &'a Quota,
    slot: u64,
    cg: Option<TerminalCgroup>,
    child: Option<Arc<Mutex<Option<Child>>>>,
    tasks: Vec<JoinHandle<()>>,
    shared: Option<Arc<WriterShared>>,
    /// Fault injection: skip cgroup.kill so the empty-verification below must
    /// fail (the slot must stay in Cleaning).
    no_kill: bool,
    /// Fault injection for the root-reap verification above.
    reap_fault: Option<ReapFault>,
}

impl Rollback<'_> {
    async fn run(mut self, error: SpawnError) -> SpawnError {
        if let Some(shared) = &self.shared {
            let mut coord = shared.coordinator.lock().unwrap();
            coord.stop_committed = true;
            let _ = shared.stop_tx.send(true);
            let _ = shared.exit_tx.send(true);
        }
        let mut failures: Vec<&'static str> = Vec::new();
        let kill_ok = if self.no_kill {
            failures.push("injected: cgroup.kill skipped");
            false
        } else {
            self.cg
                .as_ref()
                .map(|cg| cgroup::cg_kill(&cg.path).is_ok())
                .unwrap_or(true)
        };
        if !kill_ok && !self.no_kill {
            failures.push("cgroup.kill failed");
        }
        if kill_ok && let Some(child) = &self.child {
            // Reap whatever the monitor has not reaped yet. BOTH failure
            // modes are cleanup failures — a try_wait error, and a root not
            // reaped before the deadline — so the slot stays in Cleaning and
            // is never released on either.
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut reaped = false;
            let mut reap_failure: Option<&'static str> = None;
            while Instant::now() < deadline {
                if self.reap_fault == Some(ReapFault::NeverReaped) {
                    // Injected: the root is never observed reaped.
                    sleep(Duration::from_millis(10)).await;
                    continue;
                }
                let result = {
                    let mut guard = child.lock().unwrap();
                    match guard.as_mut() {
                        Some(child) => Some(if self.reap_fault == Some(ReapFault::TryWaitError) {
                            Err(std::io::Error::other("injected try_wait error"))
                        } else {
                            child.try_wait()
                        }),
                        None => None,
                    }
                };
                match result {
                    Some(Ok(Some(_))) | None => {
                        reaped = true;
                        break;
                    }
                    Some(Ok(None)) => {}
                    Some(Err(_)) => {
                        reap_failure = Some("root reap try_wait error");
                        reaped = true;
                        break;
                    }
                }
                sleep(Duration::from_millis(10)).await;
            }
            if reap_failure.is_none() && !reaped {
                reap_failure = Some("root reap timed out");
            }
            if let Some(failure) = reap_failure {
                failures.push(failure);
            }
        }
        let empty = if kill_ok {
            match self.cg.as_ref() {
                Some(cg) => cgroup::cg_wait_empty(&cg.path, Duration::from_secs(3)).await,
                None => true,
            }
        } else {
            false
        };
        if !empty {
            failures.push("terminal cgroup not empty after kill");
        }
        let mut joined = true;
        for handle in self.tasks.drain(..) {
            handle.abort();
            if tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .is_err()
            {
                joined = false;
            }
        }
        if !joined {
            failures.push("management task join timed out");
        }
        // Only an already-empty cgroup is worth removing; a failed remove
        // (e.g. still-populated) keeps the failure explicit.
        if empty
            && let Some(cg) = self.cg.take()
            && cg.remove().is_err()
        {
            failures.push("terminal cgroup remove failed");
        }
        let cleanup_ok = failures.is_empty();
        // Transition/release the reservation exactly once, and ONLY release
        // on verified cleanup; otherwise the slot stays in Cleaning.
        match self.quota.state(self.slot) {
            Some(SlotState::Reserved) => {
                let _ = if cleanup_ok {
                    self.quota.start_failed(self.slot)
                } else {
                    self.quota.begin_cleaning(self.slot)
                };
            }
            Some(SlotState::Active) => {
                let _ = self.quota.begin_cleaning(self.slot);
                if cleanup_ok {
                    let _ = self.quota.release(self.slot);
                }
            }
            _ => {}
        }
        if cleanup_ok {
            error
        } else {
            SpawnError::CleanupFailed {
                fault: format!("{error:?}"),
                detail: failures.join("; "),
            }
        }
    }
}

/// The write loop. Readiness waits, the deadline, and service-shutdown
/// cancellation live OUTSIDE the coordinator lock; the barrier check and the
/// actual bounded write syscall happen together inside one short critical
/// section, giving every byte and every generation/stop commit a total order.
#[allow(clippy::too_many_arguments)]
async fn write_nonblocking(
    fd: &AsyncFd<OwnedFd>,
    data: &[u8],
    generation: u64,
    deadline: Duration,
    progress: &AtomicUsize,
    shared: &WriterShared,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> SendOutcome {
    let deadline_at = tokio::time::Instant::from_std(Instant::now() + deadline);
    let mut stop_rx = shared.stop_tx.subscribe();
    let mut gen_rx = shared.gen_tx.subscribe();
    let mut written = 0usize;

    loop {
        // Loop-head barrier check under the lock (fast rejection paths).
        {
            let coord = shared.coordinator.lock().unwrap();
            if coord.stop_committed {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::StopIntent,
                };
            }
            if coord.generation != generation {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::ControlLost,
                };
            }
            if written == data.len() {
                return SendOutcome::Complete { written };
            }
        }

        // Readiness / cancellation waits: OUTSIDE the lock.
        let mut guard = tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                continue; // re-check under the lock at the loop head
            }
            _ = gen_rx.changed() => {
                continue;
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return SendOutcome::Aborted {
                        written,
                        reason: AbortReason::ServiceShutdown,
                    };
                }
                continue;
            }
            _ = tokio::time::sleep_until(deadline_at) => {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::WriteDeadline,
                };
            }
            ready = fd.writable() => match ready {
                Ok(guard) => guard,
                Err(e) => {
                    return SendOutcome::Aborted {
                        written,
                        reason: AbortReason::WriteFailed(format!("writable: {e}")),
                    }
                }
            },
        };

        // Short critical section: barrier check + ONE bounded write syscall,
        // linearized together. No await while the lock is held.
        let chunk_end = (written + WRITE_CHUNK).min(data.len());
        let result = {
            let mut coord = shared.coordinator.lock().unwrap();
            if coord.stop_committed {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::StopIntent,
                };
            }
            if coord.generation != generation {
                // A generation change observed before the commit is a LEGAL
                // rejection (the claim was taken before the bump committed):
                // refuse the syscall, count nothing.
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::ControlLost,
                };
            }
            let io_result =
                guard.try_io(|afd| raw_write(afd.as_raw_fd(), &data[written..chunk_end]));
            if let Ok(Ok(n)) = &io_result {
                // WriteCommit evidence: appended under the same lock as the
                // syscall, so the ordered log cannot interleave with a
                // SwitchCommit entry.
                coord.seq += 1;
                let seq = coord.seq;
                let bytes = *n as u64;
                coord.log.push(CommitEvent::Write {
                    generation,
                    seq,
                    bytes,
                });
            }
            io_result
        };
        match result {
            Ok(Ok(n)) if n > 0 => {
                written += n;
                progress.store(written, Ordering::SeqCst);
            }
            Ok(Ok(_)) => {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::WriteFailed("zero-length write".into()),
                };
            }
            Ok(Err(e)) => {
                return SendOutcome::Aborted {
                    written,
                    reason: AbortReason::WriteFailed(e.to_string()),
                };
            }
            Err(_would_block) => {
                // Spurious readiness; the guard cleared it. Wait again.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepted payloads must be normalized to exact-length boxed slices: a
    /// caller Vec with 8 MiB of spare capacity and a tiny len must not be
    /// retained — the accepted queue backs exactly `len` payload bytes
    /// (allocator metadata/rounding aside).
    #[test]
    fn accepted_payload_backing_is_exact_length() {
        let capacity = 8 * 1024 * 1024;
        let mut data = Vec::with_capacity(capacity);
        data.extend_from_slice(b"ping");
        assert!(
            data.capacity() >= capacity,
            "caller Vec must start with the 8 MiB spare capacity"
        );

        // The shrink step the normalization performs, observed on a mirror
        // Vec: shrink_to_fit drops the spare capacity to exactly len.
        let mut mirror = Vec::with_capacity(capacity);
        mirror.extend_from_slice(b"ping");
        mirror.shrink_to_fit();
        assert_eq!(mirror.capacity(), mirror.len());

        let normalized = normalize_payload(data);
        // A Box<[u8]> has no spare capacity: the backing request is exactly
        // the payload length.
        assert_eq!(normalized.len(), 4);
        assert_eq!(&normalized[..], b"ping");
    }
}
