//! The per-terminal PTY state machine.
//!
//! One terminal owns: its PTY master write fd (a duplicate wrapped in a
//! self-managed `AsyncFd`), a bounded writer queue, a reader that drains
//! the master into a bounded handoff to the S2 raw stream, a monitor that
//! reaps the root exactly once, and its per-terminal cgroup. Lifecycle
//! commits are one-shot: root exit, output close, the stop intent, cleanup
//! completion, and the quota release each happen exactly once.
//!
//! Cleanup is identity-safe by construction: the gentle phase signals only
//! current cgroup members through pidfds, the forced phase writes
//! `cgroup.kill`, and the cgroup is removed only after it is verified
//! empty. A raw pid, `pgid 0/-1`, a `/proc` snapshot, or `pgrep` is never
//! used. The quota slot is released only after cgroup empty, root reaped,
//! tasks joined, the output sink closed, and the cgroup removed — an
//! unverified cleanup leaves the slot occupied.
//!
//! A `Stop` commits its in-memory intent and returns immediately; the
//! cleanup runs in a detached task and repeated `Stop` shares it. `Stop`,
//! generation advances, and every actual write syscall linearize on the
//! same coordinator mutex.

use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pty_process::{Command, OwnedReadPty};
use qingluan_core::terminal::{
    ControlGeneration, ExitResult, LogEpoch, LogIdentity, OutputEnd, OutputState, PartialWrite,
    ProcessState, SendReceipt, StartSpec, TerminalRef, TerminalSize, TerminalSnapshot, WriteAbort,
};
use qingluan_storage::{AppendOutcome, LogStore, LogWriter, RuntimeRegistry};
use tokio::io::AsyncReadExt;
use tokio::io::unix::AsyncFd;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::cgroup::{self, DelegatedRoot, TerminalCgroup};
use crate::error::{RuntimeError, SendError, SendRejection};
use crate::limits::{
    CGROUP_KILL_WAIT, MAX_SEND_BYTES, OUTPUT_CLOSE_WAIT, OUTPUT_HANDOFF_BYTES, READ_CHUNK,
    ROOT_REAP_WAIT, SendPayload, TASK_JOIN_WAIT, TERM_GRACE, WRITE_DEADLINE, WRITE_QUEUE_CAPACITY,
};
use crate::quota::{Quota, SlotId, SlotState};
use crate::write::{CountedWrite, WriteCoordinator, WriteOutcome, write_bounded};

/// The state published by a terminal's detached cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CleanupState {
    /// No cleanup has completed yet (or a natural-end attempt is retrying).
    Pending,
    /// Cleanup finished; `Ok` means every step was verified, `Err` carries
    /// the unverified step (the quota slot stays occupied on `Err`).
    Finished(Result<(), String>),
}

/// Which detached cleanup flow won the terminal's single leader slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupLeader {
    /// An explicit `Stop` was committed.
    Stop,
    /// The terminal ended naturally (root exited and output closed).
    Finalize,
}

/// Rejection of a resize at its commit point.
enum ResizeRejection {
    Stopped,
    ControlLost,
    Failed(String),
}

/// One queued writer command.
enum WriterCommand {
    Write {
        payload: Box<[u8]>,
        generation: ControlGeneration,
        deadline: Duration,
        progress: Arc<AtomicUsize>,
        response: oneshot::Sender<Result<SendReceipt, SendError>>,
    },
    Resize {
        size: TerminalSize,
        generation: ControlGeneration,
        response: oneshot::Sender<Result<(), ResizeRejection>>,
    },
}

/// One message handed from the reader to the output sink.
enum OutputMessage {
    /// Bytes to append, preceded by an exact dropped-byte gap (if any).
    Bytes { gap_before: u64, bytes: Vec<u8> },
    /// The reader ended; `trailing_gap` are bytes dropped after the last
    /// message.
    End { trailing_gap: u64 },
}

/// Bounded handoff from the reader to the storage sink.
///
/// The reader never awaits the sink: once `queued` bytes are still
/// undrained it drops the new run and folds it into the next message's
/// `gap_before` (ordering-exact, no shared-counter race), so a slow or
/// faulted store can never indefinitely backpressure the PTY.
struct OutputHandoff {
    tx: mpsc::UnboundedSender<OutputMessage>,
    queued: Arc<AtomicU64>,
    pending_gap: u64,
}

impl OutputHandoff {
    fn offer(&mut self, bytes: &[u8]) {
        let len = bytes.len() as u64;
        if self.queued.load(Ordering::SeqCst).saturating_add(len) > OUTPUT_HANDOFF_BYTES {
            self.pending_gap = self.pending_gap.saturating_add(len);
            return;
        }
        let gap_before = std::mem::take(&mut self.pending_gap);
        let message = OutputMessage::Bytes {
            gap_before,
            bytes: bytes.to_vec(),
        };
        // Increment before sending: the sink can only observe the message
        // after the send, so its decrement can never underflow.
        self.queued.fetch_add(len, Ordering::SeqCst);
        if self.tx.send(message).is_err() {
            self.queued.fetch_sub(len, Ordering::SeqCst);
        }
    }

    fn finish(&mut self) {
        let trailing_gap = std::mem::take(&mut self.pending_gap);
        let _ = self.tx.send(OutputMessage::End { trailing_gap });
    }
}

/// The start transaction's inputs.
pub(crate) struct StartRequest {
    pub(crate) terminal: TerminalRef,
    pub(crate) generation: ControlGeneration,
    pub(crate) spec: StartSpec,
}

/// One live (or completed) terminal.
pub(crate) struct Terminal {
    terminal: TerminalRef,
    slot: SlotId,
    registry: RuntimeRegistry,
    quota: Arc<Quota>,
    coord: Arc<WriteCoordinator>,
    writer_tx: mpsc::Sender<WriterCommand>,
    exit_tx: watch::Sender<bool>,
    output_end_tx: mpsc::UnboundedSender<OutputMessage>,
    handoff_queued: Arc<AtomicU64>,
    // Retained for the `test-hooks` degraded accessor; the sink task holds
    // its own clone and is the production reader of this latch.
    #[allow(dead_code)]
    degraded: Arc<AtomicBool>,
    child: Mutex<Option<Child>>,
    cgroup: Mutex<Option<TerminalCgroup>>,
    writer_handle: Mutex<Option<JoinHandle<()>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    sink_handle: Mutex<Option<JoinHandle<Result<(), String>>>>,
    monitor_handle: Mutex<Option<JoinHandle<()>>>,
    exited: AtomicBool,
    exit_result: Mutex<Option<ExitResult>>,
    output_done: AtomicBool,
    output_end: Mutex<Option<OutputEnd>>,
    cleanup_done: AtomicBool,
    quota_released: AtomicBool,
    /// Test-only fault: consume one monitor poll as a transient `try_wait`
    /// error (never a fabricated exit). Inert unless armed through the
    /// `test-hooks` seam.
    #[cfg(any(test, feature = "test-hooks"))]
    monitor_fault: Arc<AtomicBool>,
    /// Test-only fault: fail cleanup verification after every real identity
    /// cleanup step, so the quota release is withheld. Inert unless armed.
    #[cfg(any(test, feature = "test-hooks"))]
    cleanup_fault: Arc<AtomicBool>,
    leader: Mutex<Option<CleanupLeader>>,
    cleanup: watch::Sender<CleanupState>,
    size: Mutex<TerminalSize>,
}

impl Terminal {
    /// Execute the start transaction: reserve, create the cgroup, spawn the
    /// PTY, attach the raw output writer, persist `running`, then start the
    /// reader/monitor/writer tasks. On any failure the transaction rolls
    /// back; the quota is released only when the rollback is verifiably
    /// complete.
    pub(crate) async fn start(
        store: &LogStore,
        registry: &RuntimeRegistry,
        quota: &Arc<Quota>,
        cgroup_root: &DelegatedRoot,
        shutdown_tx: &watch::Sender<bool>,
        request: StartRequest,
    ) -> Result<Arc<Terminal>, RuntimeError> {
        let StartRequest {
            terminal,
            generation,
            spec,
        } = request;

        validate_start(&terminal, &spec)?;

        let session_key = terminal.session.external_id.as_str().to_owned();
        let slot = quota
            .reserve(&session_key)
            .map_err(|error| RuntimeError::StartRejected {
                terminal: terminal.clone(),
                detail: format!("quota: {error:?}"),
            })?;

        // Reserve the durable record first: a crash during spawn leaves a
        // `starting` record that startup recovery marks Interrupted.
        if let Err(error) = registry.begin(&terminal, spec.size).await {
            let _ = quota.start_failed(slot);
            return Err(RuntimeError::StartRejected {
                terminal,
                detail: format!("registry begin: {error}"),
            });
        }

        // Everything below rolls back through `Rollback`.
        let mut rollback = Rollback {
            registry: registry.clone(),
            quota: Arc::clone(quota),
            slot,
            terminal: terminal.clone(),
            cgroup: None,
            child: None,
            writer: None,
        };

        let name = terminal.terminal_id.as_str().to_owned();
        match cgroup_root.create_terminal(&name) {
            Ok(cgroup) => rollback.cgroup = Some(cgroup),
            Err(error) => {
                return Err(rollback
                    .run(format!("create terminal cgroup: {error}"))
                    .await);
            }
        }

        // The pre-opened cgroup.procs fd for the child's async-signal-safe
        // `pre_exec` join.
        let join_fd = match rollback
            .cgroup
            .as_mut()
            .expect("cgroup present")
            .take_join_fd()
        {
            Some(fd) => fd,
            None => {
                return Err(rollback.run("cgroup join fd already taken".into()).await);
            }
        };

        let (pty, pts) = match pty_process::open() {
            Ok(pair) => pair,
            Err(error) => return Err(rollback.run(format!("open pty: {error}")).await),
        };
        if let Err(error) = pty.resize(pty_process::Size::new(spec.size.rows, spec.size.columns)) {
            return Err(rollback.run(format!("resize pty: {error}")).await);
        }
        // The write side is a dup of the master (same open file
        // description; already non-blocking), self-managed through AsyncFd.
        let writer_fd: OwnedFd = match pty.as_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(error) => return Err(rollback.run(format!("dup master fd: {error}")).await),
        };

        // Explicit environment only: env_clear first, then exactly the
        // snapshot; absolute cwd; no shell; no process_group(0) (pty-process
        // already makes the child a session leader and sets TIOCSCTTY, and
        // its pre_exec runs before ours).
        let mut command = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear();
        for (key, value) in spec.env.iter() {
            command = command.env(key, value);
        }
        // SAFETY: the closure calls only `write(2)`, which is
        // async-signal-safe; `join_fd` is a pre-opened cgroup.procs fd.
        command = unsafe {
            command.pre_exec(move || {
                let buf = b"0";
                let written = libc::write(join_fd.as_raw_fd(), buf.as_ptr().cast(), 1);
                if written == 1 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            })
        };
        let child = match command.spawn(pts) {
            Ok(child) => child,
            Err(error) => {
                return Err(rollback
                    .run(format!("spawn {}: {error}", spec.program))
                    .await);
            }
        };
        if child.id().is_none() {
            return Err(rollback.run("root exited before pid capture".into()).await);
        }
        rollback.child = Some(Arc::new(Mutex::new(Some(child))));

        // Attach the raw output writer (S2); a storage fault here fails the
        // whole start and rolls the process back.
        let log_identity = LogIdentity {
            terminal: terminal.clone(),
            log_epoch: LogEpoch::new(uuid::Uuid::now_v7().to_string()),
        };
        match store.open_writer(&log_identity).await {
            Ok(writer) => rollback.writer = Some(writer),
            Err(error) => {
                return Err(rollback.run(format!("open output writer: {error}")).await);
            }
        }

        // The durable commit of a successful start. Everything before this
        // point is rollback-safe; nothing after it can fail.
        if let Err(error) = registry.mark_running(&terminal).await {
            return Err(rollback.run(format!("persist running: {error}")).await);
        }

        let writer = rollback.writer.take().expect("writer present");
        let writer_fd = match AsyncFd::new(writer_fd) {
            Ok(fd) => fd,
            Err(error) => {
                rollback.writer = Some(writer);
                return Err(rollback.run(format!("AsyncFd writer: {error}")).await);
            }
        };

        let cgroup = rollback.cgroup.take().expect("cgroup present");
        let child_arc = rollback.child.take().expect("child present");
        let child = child_arc
            .lock()
            .expect("child")
            .take()
            .expect("child present");
        let coord = Arc::new(WriteCoordinator::new(generation));
        let (exit_tx, _exit_rx) = watch::channel(false);
        let (writer_tx, writer_rx) = mpsc::channel::<WriterCommand>(WRITE_QUEUE_CAPACITY);
        let (output_end_tx, output_rx) = mpsc::unbounded_channel::<OutputMessage>();
        let handoff_queued = Arc::new(AtomicU64::new(0));
        let degraded = Arc::new(AtomicBool::new(false));
        let (cleanup_tx, _cleanup_rx) = watch::channel(CleanupState::Pending);

        let terminal_state = Arc::new(Terminal {
            terminal,
            slot,
            registry: registry.clone(),
            quota: Arc::clone(quota),
            coord: Arc::clone(&coord),
            writer_tx,
            exit_tx: exit_tx.clone(),
            output_end_tx: output_end_tx.clone(),
            handoff_queued: Arc::clone(&handoff_queued),
            degraded: Arc::clone(&degraded),
            child: Mutex::new(Some(child)),
            cgroup: Mutex::new(Some(cgroup)),
            writer_handle: Mutex::new(None),
            reader_handle: Mutex::new(None),
            sink_handle: Mutex::new(None),
            monitor_handle: Mutex::new(None),
            exited: AtomicBool::new(false),
            exit_result: Mutex::new(None),
            output_done: AtomicBool::new(false),
            output_end: Mutex::new(None),
            cleanup_done: AtomicBool::new(false),
            quota_released: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-hooks"))]
            monitor_fault: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-hooks"))]
            cleanup_fault: Arc::new(AtomicBool::new(false)),
            leader: Mutex::new(None),
            cleanup: cleanup_tx,
            size: Mutex::new(spec.size),
        });

        let (reader, _write_half) = pty.into_split();

        let reader_task = {
            let terminal_state = Arc::clone(&terminal_state);
            tokio::spawn(async move { run_reader(terminal_state, reader).await })
        };
        terminal_state
            .reader_handle
            .lock()
            .expect("reader handle")
            .replace(reader_task);

        let sink_task = {
            let queued = Arc::clone(&handoff_queued);
            let degraded = Arc::clone(&degraded);
            tokio::spawn(async move { run_output_sink(writer, output_rx, queued, degraded).await })
        };
        terminal_state
            .sink_handle
            .lock()
            .expect("sink handle")
            .replace(sink_task);

        let monitor_task = {
            let terminal_state = Arc::clone(&terminal_state);
            tokio::spawn(async move { run_monitor(terminal_state).await })
        };
        terminal_state
            .monitor_handle
            .lock()
            .expect("monitor handle")
            .replace(monitor_task);

        let writer_task = {
            let coord = Arc::clone(&coord);
            let shutdown_rx = shutdown_tx.subscribe();
            let exit_rx = exit_tx.subscribe();
            tokio::spawn(async move {
                run_writer(writer_fd, writer_rx, coord, shutdown_rx, exit_rx).await
            })
        };
        terminal_state
            .writer_handle
            .lock()
            .expect("writer handle")
            .replace(writer_task);

        Ok(terminal_state)
    }

    /// Arm the one-shot monitor fault (test builds only). The next monitor
    /// poll is treated as a transient `try_wait` error: it must not commit
    /// an exit or set the exited flag.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn arm_monitor_fault(&self) {
        self.monitor_fault.store(true, Ordering::SeqCst);
    }

    /// Arm the one-shot cleanup-verification fault (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn arm_cleanup_fault(&self) {
        self.cleanup_fault.store(true, Ordering::SeqCst);
    }

    /// Reconcile a withheld cleanup release (test builds only): persist
    /// `released`, free the quota slot exactly once, and publish success.
    /// Used after proving the fault withheld the release, so teardown is
    /// clean.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) async fn reconcile_cleanup_failure(&self) {
        let _ = self.registry.released(&self.terminal).await;
        self.release_quota();
        self.cleanup_done.store(true, Ordering::SeqCst);
        let _ = self.cleanup.send_replace(CleanupState::Finished(Ok(())));
    }

    /// The terminal's identity.
    pub(crate) fn reference(&self) -> &TerminalRef {
        &self.terminal
    }

    /// Whether the output sink latched degraded.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    /// A point-in-time snapshot of the terminal's independent dimensions.
    pub(crate) fn snapshot(&self) -> TerminalSnapshot {
        let process = match *self.exit_result.lock().expect("exit result") {
            Some(result) => ProcessState::Exited(result),
            None => ProcessState::Running,
        };
        let output = match *self.output_end.lock().expect("output end") {
            Some(end) => OutputState::Closed(end),
            None => OutputState::Open,
        };
        TerminalSnapshot {
            terminal: self.terminal.clone(),
            process,
            output,
            stopping: self.coord.is_stopped() && !self.cleanup_done.load(Ordering::SeqCst),
            size: *self.size.lock().expect("size"),
            retained_history: None,
        }
    }

    /// Commit the stop intent and, on the first call, start the detached
    /// cleanup. Returns immediately; repeated calls share the same
    /// cleanup.
    pub(crate) fn stop(self: &Arc<Self>) {
        if self.cleanup_done.load(Ordering::SeqCst) {
            return;
        }
        if self.claim(CleanupLeader::Stop) {
            self.coord.commit_stop();
            let terminal = Arc::clone(self);
            tokio::spawn(async move { terminal.run_stop().await });
        }
    }

    /// Bounded send: validates the size, refuses stale/stopped requests at
    /// their commit point, and reports the exact written count.
    pub(crate) async fn send(
        self: &Arc<Self>,
        generation: ControlGeneration,
        data: Vec<u8>,
    ) -> Result<SendReceipt, SendError> {
        if data.len() > MAX_SEND_BYTES {
            return Err(SendError::Rejected(SendRejection::Oversize));
        }
        let payload = match SendPayload::accept(data) {
            Ok(payload) => payload,
            Err(_) => return Err(SendError::Rejected(SendRejection::Oversize)),
        };
        match self.coord.check(generation) {
            Err(WriteAbort::StopIntent) => return Err(SendError::Rejected(SendRejection::Stopped)),
            Err(WriteAbort::ControlLost) => {
                return Err(SendError::Rejected(SendRejection::ControlLost));
            }
            _ => {}
        }
        let (response, receiver) = oneshot::channel();
        let progress = Arc::new(AtomicUsize::new(0));
        let command = WriterCommand::Write {
            payload: payload.into_bytes(),
            generation,
            deadline: WRITE_DEADLINE,
            progress,
            response,
        };
        match self.writer_tx.try_send(command) {
            Ok(()) => match receiver.await {
                Ok(result) => result,
                Err(_) => Err(SendError::Rejected(SendRejection::Stopped)),
            },
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(SendError::Rejected(SendRejection::QueueFull))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(SendError::Rejected(SendRejection::Stopped))
            }
        }
    }

    /// Apply a resize, ordered against sends and committed with the same
    /// generation check.
    pub(crate) async fn resize(
        self: &Arc<Self>,
        generation: ControlGeneration,
        size: TerminalSize,
    ) -> Result<(), RuntimeError> {
        let (response, receiver) = oneshot::channel();
        let command = WriterCommand::Resize {
            size,
            generation,
            response,
        };
        match self.writer_tx.try_send(command) {
            Ok(()) => match receiver.await {
                Ok(Ok(())) => {
                    *self.size.lock().expect("size") = size;
                    Ok(())
                }
                Ok(Err(ResizeRejection::ControlLost)) => {
                    Err(RuntimeError::ControlLost(self.terminal.clone()))
                }
                Ok(Err(ResizeRejection::Stopped)) | Err(_) => {
                    Err(RuntimeError::NotWritable(self.terminal.clone()))
                }
                Ok(Err(ResizeRejection::Failed(detail))) => Err(RuntimeError::Io { detail }),
            },
            Err(_) => Err(RuntimeError::NotWritable(self.terminal.clone())),
        }
    }

    /// Await the detached cleanup's published state.
    pub(crate) async fn wait_cleanup(&self) -> CleanupState {
        let mut receiver = self.cleanup.subscribe();
        loop {
            let state = receiver.borrow_and_update().clone();
            if matches!(state, CleanupState::Finished(_)) {
                return state;
            }
            if receiver.changed().await.is_err() {
                return CleanupState::Pending;
            }
        }
    }

    fn claim(&self, leader: CleanupLeader) -> bool {
        let mut guard = self.leader.lock().expect("cleanup leader");
        if guard.is_none() {
            *guard = Some(leader);
            true
        } else {
            false
        }
    }

    /// Advance the terminal's write generation (called by a session-level
    /// control takeover). Linearized with every actual write syscall.
    pub(crate) fn bump_generation(&self, next: ControlGeneration) {
        self.coord.advance_generation(next);
    }

    fn cgroup_path(&self) -> Option<PathBuf> {
        self.cgroup
            .lock()
            .expect("cgroup")
            .as_ref()
            .map(|cgroup| cgroup.path().to_path_buf())
    }

    fn new_handoff(&self) -> OutputHandoff {
        OutputHandoff {
            tx: self.output_end_tx.clone(),
            queued: Arc::clone(&self.handoff_queued),
            pending_gap: 0,
        }
    }

    async fn record_output_end(self: &Arc<Self>, end: OutputEnd) {
        if self.output_done.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.output_end.lock().expect("output end") = Some(end);
        let _ = self.registry.output_close(&self.terminal, end).await;
        self.maybe_finalize();
    }

    async fn record_process_exit(self: &Arc<Self>, result: ExitResult) {
        if self.exited.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.exit_result.lock().expect("exit result") = Some(result);
        let _ = self.registry.process_exit(&self.terminal, result).await;
        self.maybe_finalize();
    }

    /// Trigger the natural-end finalize once the root has exited and the
    /// output has closed. Only runs when the whole cgroup is already empty;
    /// a terminal with live members needs an explicit `Stop`.
    fn maybe_finalize(self: &Arc<Self>) {
        if !(self.exited.load(Ordering::SeqCst) && self.output_done.load(Ordering::SeqCst)) {
            return;
        }
        if self.cleanup_done.load(Ordering::SeqCst) {
            return;
        }
        if let Some(path) = self.cgroup_path()
            && cg_populated(&path)
        {
            return;
        }
        if self.claim(CleanupLeader::Finalize) {
            let terminal = Arc::clone(self);
            tokio::spawn(async move { terminal.run_finalize().await });
        }
    }

    async fn run_stop(self: Arc<Self>) {
        let mut failures: Vec<String> = Vec::new();

        if let Err(error) = self.registry.stop_intent(&self.terminal).await {
            failures.push(format!("persist stop intent: {error}"));
        }

        // Gentle phase: identity-safe SIGTERM to current cgroup members,
        // rescanning so TERM-time forks are covered, up to the grace.
        if let Some(path) = self.cgroup_path() {
            let deadline = Instant::now() + TERM_GRACE;
            while cg_populated(&path) && Instant::now() < deadline {
                let _ = cgroup::term_members(&path);
                sleep(Duration::from_millis(20)).await;
            }
        }

        // Forced phase: cgroup.kill is the fork-safe fixed point.
        let graceful = self
            .cgroup_path()
            .map(|path| !cg_populated(&path))
            .unwrap_or(true);
        if !graceful && let Some(path) = self.cgroup_path() {
            if let Err(error) = cgroup::kill(&path) {
                failures.push(format!("cgroup.kill: {error}"));
            }
            if !cgroup::wait_empty(&path, CGROUP_KILL_WAIT).await {
                failures.push("terminal cgroup not empty after cgroup.kill".into());
            }
        }

        // The monitor reaps the root; wait for its one-shot commit.
        let reap_deadline = Instant::now() + ROOT_REAP_WAIT;
        while !self.exited.load(Ordering::SeqCst) {
            if Instant::now() >= reap_deadline {
                failures.push("root process not reaped".into());
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }

        // Output must end within the bound; the timeout is the unique
        // ForcedClose winner.
        let output_deadline = Instant::now() + OUTPUT_CLOSE_WAIT;
        while !self.output_done.load(Ordering::SeqCst) {
            if Instant::now() >= output_deadline {
                self.force_close_output();
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        if !self.output_done.load(Ordering::SeqCst) {
            failures.push("output not closed".into());
        }

        self.finish_resources(&mut failures).await;
        self.publish(failures);
    }

    async fn run_finalize(self: Arc<Self>) {
        let mut failures: Vec<String> = Vec::new();
        self.finish_resources(&mut failures).await;
        self.publish(failures);
    }

    /// Join management tasks, remove the (verified empty) cgroup, and —
    /// only when every step is verified — persist `released` and free the
    /// quota slot exactly once.
    async fn finish_resources(&self, failures: &mut Vec<String>) {
        let _ = self.exit_tx.send(true);
        self.join_tasks(failures).await;

        if let Some(cgroup) = self.cgroup.lock().expect("cgroup").take() {
            if cg_populated(cgroup.path()) {
                failures.push("terminal cgroup still populated".into());
                *self.cgroup.lock().expect("cgroup") = Some(cgroup);
            } else if let Err(error) = cgroup.remove() {
                failures.push(format!("remove terminal cgroup: {error}"));
            }
        }

        if !failures.is_empty() {
            return;
        }
        // Test hook: every real identity cleanup step above has run (root
        // reaped, tasks joined, cgroup verified empty and removed). The
        // injected verification failure must still withhold the release.
        #[cfg(any(test, feature = "test-hooks"))]
        if self.cleanup_fault.swap(false, Ordering::SeqCst) {
            failures.push("injected cleanup verification failure (test hook)".into());
            return;
        }
        if let Err(error) = self.registry.released(&self.terminal).await {
            failures.push(format!("persist released: {error}"));
            return;
        }
        self.release_quota();
        self.cleanup_done.store(true, Ordering::SeqCst);
    }

    async fn join_tasks(&self, failures: &mut Vec<String>) {
        let reader = self.reader_handle.lock().expect("reader handle").take();
        join_unit(reader, "reader", failures).await;
        let monitor = self.monitor_handle.lock().expect("monitor handle").take();
        join_unit(monitor, "monitor", failures).await;
        let writer = self.writer_handle.lock().expect("writer handle").take();
        join_unit(writer, "writer", failures).await;
        let sink = self.sink_handle.lock().expect("sink handle").take();
        if let Some(handle) = sink {
            match tokio::time::timeout(TASK_JOIN_WAIT, handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(detail))) => failures.push(format!("output sink: {detail}")),
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => failures.push(format!("output sink task failed: {error}")),
                Err(_) => failures.push("output sink join timed out".into()),
            }
        }
    }

    fn force_close_output(self: &Arc<Self>) {
        if self.output_done.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(handle) = self.reader_handle.lock().expect("reader handle").as_ref() {
            handle.abort();
        }
        *self.output_end.lock().expect("output end") = Some(OutputEnd::ForcedClose);
        let terminal = Arc::clone(self);
        tokio::spawn(async move {
            let _ = terminal
                .output_end_tx
                .send(OutputMessage::End { trailing_gap: 0 });
            let _ = terminal
                .registry
                .output_close(&terminal.terminal, OutputEnd::ForcedClose)
                .await;
        });
    }

    fn publish(&self, failures: Vec<String>) {
        let state = if failures.is_empty() {
            CleanupState::Finished(Ok(()))
        } else {
            CleanupState::Finished(Err(failures.join("; ")))
        };
        let _ = self.cleanup.send_replace(state);
    }

    fn release_quota(&self) {
        if self.quota_released.swap(true, Ordering::SeqCst) {
            return;
        }
        match self.quota.state(self.slot) {
            Some(SlotState::Active) => {
                let _ = self.quota.begin_cleaning(self.slot);
                let _ = self.quota.release(self.slot);
            }
            Some(SlotState::Reserved) => {
                let _ = self.quota.start_failed(self.slot);
            }
            _ => {}
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Safety net: wake every task so nothing waits forever if the
        // runtime is dropped without an explicit shutdown.
        self.coord.commit_stop();
        let _ = self.exit_tx.send(true);
        let _ = self
            .output_end_tx
            .send(OutputMessage::End { trailing_gap: 0 });
    }
}

async fn join_unit(handle: Option<JoinHandle<()>>, name: &str, failures: &mut Vec<String>) {
    if let Some(handle) = handle {
        match tokio::time::timeout(TASK_JOIN_WAIT, handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => failures.push(format!("{name} task failed: {error}")),
            Err(_) => failures.push(format!("{name} task join timed out")),
        }
    }
}

/// Whether the cgroup at `path` currently holds live processes; an
/// unreadable `cgroup.events` is treated as populated (never silently
/// empty).
fn cg_populated(path: &std::path::Path) -> bool {
    cgroup::populated(path).unwrap_or(true)
}

fn validate_start(terminal: &TerminalRef, spec: &StartSpec) -> Result<(), RuntimeError> {
    if spec.program.is_empty() {
        return Err(RuntimeError::StartRejected {
            terminal: terminal.clone(),
            detail: "program is empty".into(),
        });
    }
    let cwd = std::path::Path::new(&spec.cwd);
    if !cwd.is_absolute() {
        return Err(RuntimeError::StartRejected {
            terminal: terminal.clone(),
            detail: format!("cwd is not absolute: {}", spec.cwd),
        });
    }
    if !cwd.is_dir() {
        return Err(RuntimeError::StartRejected {
            terminal: terminal.clone(),
            detail: format!("cwd does not exist: {}", spec.cwd),
        });
    }
    if spec.size.rows == 0 || spec.size.columns == 0 {
        return Err(RuntimeError::StartRejected {
            terminal: terminal.clone(),
            detail: "terminal size must be non-zero".into(),
        });
    }
    Ok(())
}

async fn run_reader(terminal: Arc<Terminal>, mut reader: OwnedReadPty) {
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut handoff = terminal.new_handoff();
    let end = loop {
        match reader.read(&mut buffer).await {
            // Linux: the last-slave-close read returns EIO, not EOF; both
            // are a normal end (S3 verified).
            Ok(0) => break OutputEnd::Eof,
            Ok(n) => handoff.offer(&buffer[..n]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break OutputEnd::Eof,
            Err(_) => break OutputEnd::ReadError,
        }
    };
    handoff.finish();
    terminal.record_output_end(end).await;
}

async fn run_output_sink(
    mut writer: LogWriter,
    mut receiver: mpsc::UnboundedReceiver<OutputMessage>,
    queued: Arc<AtomicU64>,
    degraded: Arc<AtomicBool>,
) -> Result<(), String> {
    while let Some(message) = receiver.recv().await {
        match message {
            OutputMessage::Bytes { gap_before, bytes } => {
                if gap_before > 0
                    && !degraded.load(Ordering::SeqCst)
                    && writer.record_raw_loss(gap_before).await.is_err()
                {
                    degraded.store(true, Ordering::SeqCst);
                }
                if !degraded.load(Ordering::SeqCst) {
                    match writer.append_raw(&bytes).await {
                        Ok(appended) => {
                            if matches!(appended.outcome, AppendOutcome::Dropped) {
                                degraded.store(true, Ordering::SeqCst);
                            }
                        }
                        Err(_) => degraded.store(true, Ordering::SeqCst),
                    }
                }
                queued.fetch_sub(bytes.len() as u64, Ordering::SeqCst);
            }
            OutputMessage::End { trailing_gap } => {
                if trailing_gap > 0 && !degraded.load(Ordering::SeqCst) {
                    let _ = writer.record_raw_loss(trailing_gap).await;
                }
                break;
            }
        }
    }
    writer
        .close()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn run_monitor(terminal: Arc<Terminal>) {
    loop {
        // Test hook: consume one injected transient failure. A monitor
        // error commits no exit and sets no flag; the loop backs off and
        // retries, exactly like a real `try_wait` error.
        #[cfg(any(test, feature = "test-hooks"))]
        if terminal.monitor_fault.swap(false, Ordering::SeqCst) {
            sleep(Duration::from_millis(50)).await;
            continue;
        }
        let result = {
            let mut guard = terminal.child.lock().expect("child");
            match guard.as_mut() {
                Some(child) => child.try_wait(),
                None => return,
            }
        };
        match result {
            Ok(Some(status)) => {
                if let Some(result) = exit_result_of(&status) {
                    terminal.record_process_exit(result).await;
                }
                return;
            }
            Ok(None) => sleep(Duration::from_millis(15)).await,
            Err(_) => sleep(Duration::from_millis(50)).await,
        }
    }
}

fn exit_result_of(status: &std::process::ExitStatus) -> Option<ExitResult> {
    match (status.code(), status.signal()) {
        (Some(code), _) => Some(ExitResult::ExitCode(code)),
        (None, Some(signal)) => Some(ExitResult::Signal(signal)),
        (None, None) => None,
    }
}

async fn run_writer(
    fd: AsyncFd<OwnedFd>,
    mut receiver: mpsc::Receiver<WriterCommand>,
    coord: Arc<WriteCoordinator>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut exit_rx: watch::Receiver<bool>,
) {
    let mut stop_rx = coord.stop_receiver();
    loop {
        // Every watch sender here only ever publishes `true` (stop intent,
        // cleanup exit, service shutdown) or is dropped with the runtime, so
        // any resolution means "stop draining and exit". The branches are
        // unguarded on purpose: a guarded `changed()` would never register a
        // wake-up when its guard was false at select construction.
        let command = tokio::select! {
            biased;
            _ = stop_rx.changed() => {
                drain_rejections(&mut receiver, SendRejection::Stopped);
                break;
            }
            _ = exit_rx.changed() => {
                drain_rejections(&mut receiver, SendRejection::Stopped);
                break;
            }
            _ = shutdown_rx.changed() => {
                drain_rejections(&mut receiver, SendRejection::Stopped);
                break;
            }
            command = receiver.recv() => match command {
                Some(command) => command,
                None => break,
            },
        };
        match command {
            WriterCommand::Write {
                payload,
                generation,
                deadline,
                progress,
                response,
            } => {
                let mut write_shutdown = shutdown_rx.clone();
                let counted = write_bounded(
                    &fd,
                    &payload,
                    generation,
                    deadline,
                    &coord,
                    &mut write_shutdown,
                    &progress,
                )
                .await;
                let _ = response.send(map_counted(counted));
            }
            WriterCommand::Resize {
                size,
                generation,
                response,
            } => {
                let result = resize_committed(&fd, size, generation, &coord);
                let _ = response.send(result);
            }
        }
    }
}

fn map_counted(counted: CountedWrite) -> Result<SendReceipt, SendError> {
    match counted.outcome {
        WriteOutcome::Complete => Ok(SendReceipt::new(counted.written as u64)),
        WriteOutcome::Deadline => Err(SendError::Partial(PartialWrite::new(
            counted.written as u64,
            WriteAbort::WriteDeadline,
        ))),
        WriteOutcome::Aborted(reason) => Err(SendError::Partial(PartialWrite::new(
            counted.written as u64,
            reason,
        ))),
    }
}

fn resize_committed(
    fd: &AsyncFd<OwnedFd>,
    size: TerminalSize,
    generation: ControlGeneration,
    coord: &WriteCoordinator,
) -> Result<(), ResizeRejection> {
    // The generation/stop check and the ioctl commit together under the
    // coordinator mutex, exactly like a write syscall.
    match coord.commit(generation, || {
        set_winsize(fd.as_raw_fd(), size.rows, size.columns)
    }) {
        Err(WriteAbort::StopIntent) => Err(ResizeRejection::Stopped),
        Err(WriteAbort::ControlLost) => Err(ResizeRejection::ControlLost),
        Err(_) => Err(ResizeRejection::Stopped),
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(ResizeRejection::Failed(error.to_string())),
    }
}

fn set_winsize(fd: RawFd, rows: u16, columns: u16) -> io::Result<()> {
    nix::ioctl_write_ptr_bad!(set_winsize_ioctl, libc::TIOCSWINSZ, libc::winsize);
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `fd` is a live pty master and `winsize` is a valid winsize.
    unsafe { set_winsize_ioctl(fd, &winsize) }
        .map(|_| ())
        .map_err(io::Error::from)
}

fn drain_rejections(receiver: &mut mpsc::Receiver<WriterCommand>, reason: SendRejection) {
    while let Ok(command) = receiver.try_recv() {
        match command {
            WriterCommand::Write { response, .. } => {
                let _ = response.send(Err(SendError::Rejected(reason)));
            }
            WriterCommand::Resize { response, .. } => {
                let _ = response.send(Err(ResizeRejection::Stopped));
            }
        }
    }
}

/// A startup rollback guard: every resource acquired after the durable
/// reservation is reclaimed on failure. The quota is released only when
/// the cleanup is verifiably complete.
struct Rollback {
    registry: RuntimeRegistry,
    quota: Arc<Quota>,
    slot: SlotId,
    terminal: TerminalRef,
    cgroup: Option<TerminalCgroup>,
    child: Option<Arc<Mutex<Option<Child>>>>,
    writer: Option<LogWriter>,
}

impl Rollback {
    async fn run(mut self, detail: String) -> RuntimeError {
        let mut failures: Vec<String> = Vec::new();

        // Kill by cgroup identity (never a pid/pgid/proc snapshot).
        let kill_ok = match &self.cgroup {
            Some(cgroup) => match cgroup::kill(cgroup.path()) {
                Ok(()) => true,
                Err(error) => {
                    failures.push(format!("cgroup.kill: {error}"));
                    false
                }
            },
            None => true,
        };

        // Reap the child (the monitor may already have reaped it).
        if kill_ok && let Some(child) = &self.child {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let reaped = {
                    let mut guard = child.lock().expect("child");
                    match guard.as_mut() {
                        Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                        None => true,
                    }
                };
                if reaped {
                    break;
                }
                if Instant::now() >= deadline {
                    failures.push("root not reaped during rollback".into());
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        }

        // The cgroup must be empty before it is removed.
        let empty = if kill_ok {
            match &self.cgroup {
                Some(cgroup) => cgroup::wait_empty(cgroup.path(), Duration::from_secs(3)).await,
                None => true,
            }
        } else {
            false
        };
        if !empty {
            failures.push("terminal cgroup not empty after kill".into());
        }

        // Close the output sink before the cgroup is removed.
        if let Some(writer) = self.writer.take()
            && let Err(error) = writer.close().await
        {
            failures.push(format!("close output writer: {error}"));
        }

        let cleanup_ok = if empty {
            match self.cgroup.take() {
                Some(cgroup) => match cgroup.remove() {
                    Ok(()) => true,
                    Err(error) => {
                        failures.push(format!("remove terminal cgroup: {error}"));
                        false
                    }
                },
                None => true,
            }
        } else {
            false
        };

        if !cleanup_ok {
            if failures.is_empty() {
                failures.push("cleanup not verified".into());
            }
            // Uncertain cleanup: keep the slot occupied and the record on
            // its `starting`/`cleaning` path (never released).
            let _ = self.quota.begin_cleaning(self.slot);
            let _ = self.registry.stop_intent(&self.terminal).await;
            return RuntimeError::CleanupIncomplete {
                terminal: self.terminal,
                detail: format!("{detail} (rollback: {})", failures.join("; ")),
            };
        }

        // Verified cleanup: release the reservation exactly once.
        let _ = self.registry.rollback_starting(&self.terminal).await;
        self.release_quota();
        RuntimeError::StartRejected {
            terminal: self.terminal,
            detail,
        }
    }

    fn release_quota(&self) {
        match self.quota.state(self.slot) {
            Some(SlotState::Reserved) => {
                let _ = self.quota.start_failed(self.slot);
            }
            Some(SlotState::Active) => {
                let _ = self.quota.begin_cleaning(self.slot);
                let _ = self.quota.release(self.slot);
            }
            _ => {}
        }
    }
}
