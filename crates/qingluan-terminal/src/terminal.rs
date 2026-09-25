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
    ControlGeneration, ExitResult, HistoryPosition, LogEpoch, LogIdentity, OutputEnd, OutputState,
    PartialWrite, ProcessState, ReadLimits, ReadRequest, ReadStart, SendReceipt, StartSpec, TailId,
    TailPosition, TailView, TerminalRef, TerminalSize, TerminalSnapshot, WriteAbort,
};
use qingluan_storage::{
    AppendOutcome, LogStore, LogStream, LogWriter, RuntimeRegistry, StreamFlushOutcome,
};
use tokio::io::AsyncReadExt;
use tokio::io::unix::AsyncFd;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::cgroup::{self, DelegatedRoot, TerminalCgroup};
use crate::error::{
    QuotaScope, RuntimeError, SendError, SendRejection, query_error, storage_error,
};
use crate::limits::{
    CGROUP_KILL_WAIT, MAX_SEND_BYTES, OUTPUT_CLOSE_WAIT, OUTPUT_HANDOFF_BYTES, READ_CHUNK,
    ROOT_REAP_WAIT, SendPayload, TASK_JOIN_WAIT, TERM_GRACE, WRITE_DEADLINE, WRITE_QUEUE_CAPACITY,
};
use crate::normalize::{Normalizer, PendingLine, TailState};
use crate::quota::{Quota, QuotaError, SlotId, SlotState};
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
    /// A query asked for one consistent cut of committed history plus the
    /// mutable tail (see [`tail_view`]). Because this message rides the
    /// same ordered queue as the output bytes, every byte queued before it
    /// is normalized before the cut is taken, and no later byte can enter
    /// it. The reply is a plain domain value: no writer, parser, or channel
    /// type crosses the seam.
    Checkpoint {
        limits: ReadLimits,
        response: oneshot::Sender<Result<TailView, String>>,
    },
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
    /// The log identity this terminal's output is persisted under (the
    /// writer was opened with it, and the query surface resolves it).
    log: LogIdentity,
    /// The bounded mutable tail of the normalized stream, shared with the
    /// output sink and read by the query surface.
    tail: Arc<Mutex<TailState>>,
    slot: SlotId,
    registry: RuntimeRegistry,
    quota: Arc<Quota>,
    coord: Arc<WriteCoordinator>,
    writer_tx: mpsc::Sender<WriterCommand>,
    exit_tx: watch::Sender<bool>,
    output_end_tx: mpsc::UnboundedSender<OutputMessage>,
    /// Published (or dropped) when the output sink has finished: its
    /// writer is closed, so every accepted line is durable and the tail can
    /// no longer change. A tail checkpoint waits on this instead of trusting
    /// `OutputState::Closed`, which the reader commits before the sink has
    /// flushed.
    sink_done: watch::Receiver<SinkOutcome>,
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
        let slot = quota.reserve(&session_key).map_err(|error| match error {
            QuotaError::SessionExhausted => RuntimeError::QuotaExhausted {
                scope: QuotaScope::Session,
            },
            QuotaError::GlobalExhausted => RuntimeError::QuotaExhausted {
                scope: QuotaScope::Global,
            },
            other => RuntimeError::StartRejected {
                terminal: terminal.clone(),
                detail: format!("quota: {other:?}"),
            },
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
        let (sink_done_tx, sink_done_rx) = watch::channel(SinkOutcome::Running);
        // The mutable tail continues the log exactly where the writer is:
        // line numbers are never reused across a restart.
        let tail = Arc::new(Mutex::new(TailState::new(
            log_identity.clone(),
            TailId::new(uuid::Uuid::now_v7().to_string()),
            writer.line_watermark().saturating_add(1),
        )));

        let terminal_state = Arc::new(Terminal {
            terminal,
            log: log_identity.clone(),
            tail: Arc::clone(&tail),
            slot,
            registry: registry.clone(),
            quota: Arc::clone(quota),
            coord: Arc::clone(&coord),
            writer_tx,
            exit_tx: exit_tx.clone(),
            output_end_tx: output_end_tx.clone(),
            sink_done: sink_done_rx.clone(),
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
            let sink = OutputSink {
                writer,
                // The sink owns a handle to the same store the runtime
                // reads through, so a checkpoint can read history and
                // sample the tail inside one order.
                store: store.clone(),
                log: log_identity,
                queued: Arc::clone(&handoff_queued),
                degraded: Arc::clone(&degraded),
                unaccounted: Arc::new(AtomicBool::new(false)),
                tail,
                done: sink_done_tx,
            };
            tokio::spawn(async move { sink.run(output_rx).await })
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

    /// One consistent cut of this terminal's committed history plus its
    /// mutable tail (see [`sink_tail_cut`]): the request is queued behind
    /// the output the reader already handed over, and the sink takes the
    /// cut in its own single-threaded order.
    pub(crate) async fn tail_view(
        &self,
        store: &LogStore,
        limits: ReadLimits,
    ) -> Result<TailView, RuntimeError> {
        sink_tail_cut(
            &self.output_end_tx,
            &self.sink_done,
            store,
            &self.log,
            &self.tail,
            limits,
        )
        .await
    }

    /// Resolve an old tail position onto the stable history line it became.
    /// An overwritten revision, an unknown tail, or an offset inside an
    /// omitted prefix is an explicit expiry, never a silent splice.
    pub(crate) fn resolve_tail(
        &self,
        position: &TailPosition,
    ) -> Result<HistoryPosition, RuntimeError> {
        self.tail
            .lock()
            .expect("tail state")
            .resolve(position)
            .ok_or(RuntimeError::CursorExpired {
                earliest: None,
                missing: None,
            })
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

/// How the output sink ended, as far as a later query may rely on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SinkOutcome {
    /// The sink is still running.
    Running,
    /// The sink ended after a *verified* close: its writer closed
    /// successfully and every normalized line it accepted is either
    /// committed or explicitly recorded as a dropped gap. The frozen state
    /// is a consistent cut on its own.
    Durable,
    /// The sink ended without that verification — a failed close, a panic,
    /// an aborted task, or normalized data that was neither committed nor
    /// recorded as dropped. The frozen state must not be served as a cut.
    Unverified,
}

/// Everything the output sink owns: the writer it persists through, the
/// store it may read back for a consistent cut, and the shared liveness
/// handles of the terminal.
struct OutputSink {
    writer: LogWriter,
    /// The same store the runtime reads through, so a checkpoint can read
    /// committed history and sample the tail inside one message order.
    store: LogStore,
    log: LogIdentity,
    queued: Arc<AtomicU64>,
    /// The sink gave up persisting (a failure or an accounted drop): live
    /// cuts are refused and nothing more is appended.
    degraded: Arc<AtomicBool>,
    /// Some normalized line was neither committed nor explicitly recorded
    /// as a dropped gap, so the frozen final state is not a verified cut.
    unaccounted: Arc<AtomicBool>,
    tail: Arc<Mutex<TailState>>,
    /// Published when this sink has finished, and how.
    done: watch::Sender<SinkOutcome>,
}

impl OutputSink {
    async fn run(
        mut self,
        mut receiver: mpsc::UnboundedReceiver<OutputMessage>,
    ) -> Result<(), String> {
        // The guard fires on every exit path — a panic or an aborted task
        // included — and marks the end unverified unless this function
        // published a verified close itself, so a waiting query is never
        // left hanging and never falls back onto an unverified state.
        let _finish = SinkFinish(self.done.clone());
        let mut normalizer = Normalizer::new(Arc::clone(&self.tail));
        while let Some(message) = receiver.recv().await {
            match message {
                OutputMessage::Bytes { gap_before, bytes } => {
                    if gap_before > 0 {
                        // The raw archival stream records the exact loss
                        // (unchanged behavior).
                        if !self.degraded.load(Ordering::SeqCst)
                            && self.writer.record_raw_loss(gap_before).await.is_err()
                        {
                            self.degraded.store(true, Ordering::SeqCst);
                        }
                        // The dropped bytes may have contained line breaks,
                        // so the normalized stream's continuity is not
                        // knowable: commit the bytes that really were
                        // written and leave the loss to the terminal's
                        // explicit degraded latch instead of inventing a
                        // normalized line count.
                        normalizer.discontinuity();
                    }
                    if !self.degraded.load(Ordering::SeqCst) {
                        match self.writer.append_raw(&bytes).await {
                            Ok(appended) => {
                                if matches!(appended.outcome, AppendOutcome::Dropped) {
                                    self.degraded.store(true, Ordering::SeqCst);
                                }
                            }
                            Err(_) => self.degraded.store(true, Ordering::SeqCst),
                        }
                    }
                    normalizer.feed(&bytes);
                    drain_normalized(
                        &mut normalizer,
                        &mut self.writer,
                        &self.degraded,
                        &self.unaccounted,
                    )
                    .await;
                    self.queued.fetch_sub(bytes.len() as u64, Ordering::SeqCst);
                }
                OutputMessage::Checkpoint { limits, response } => {
                    let view = self.checkpoint(&mut normalizer, limits).await;
                    let _ = response.send(view);
                }
                OutputMessage::End { trailing_gap } => {
                    if trailing_gap > 0 && !self.degraded.load(Ordering::SeqCst) {
                        let _ = self.writer.record_raw_loss(trailing_gap).await;
                        normalizer.discontinuity();
                    }
                    // The end of output fixes a non-empty tail as exactly
                    // one history line, whichever way the output ended (a
                    // trailing LF-less line is real output, not a
                    // fabricated line).
                    normalizer.finish();
                    drain_normalized(
                        &mut normalizer,
                        &mut self.writer,
                        &self.degraded,
                        &self.unaccounted,
                    )
                    .await;
                    break;
                }
            }
        }
        // The close is the last durability route: it drains and reports each
        // stream's final outcome (its own last batch-bearing outcome when the
        // final drain found nothing). Only *that* result, with no
        // unaccounted normalized data, may publish a durable finish; anything
        // else leaves the mappings unpublished and the end unverified.
        let closed = self.writer.close().await;
        match &closed {
            Ok(outcomes) => {
                apply_durability(
                    &mut normalizer,
                    outcomes.normalized,
                    StreamFlushOutcome::Nothing,
                    &self.degraded,
                    &self.unaccounted,
                );
                if self.unaccounted.load(Ordering::SeqCst) {
                    self.done.send_replace(SinkOutcome::Unverified);
                } else {
                    self.done.send_replace(SinkOutcome::Durable);
                }
            }
            Err(_) => {
                self.done.send_replace(SinkOutcome::Unverified);
            }
        }
        closed.map(|_| ()).map_err(|error| error.to_string())
    }

    /// Serve one consistent cut: the newest committed history plus the
    /// mutable tail, with every line in exactly one of the two.
    ///
    /// Everything the reader queued before this checkpoint has already been
    /// normalized by this sink, and this sink is the only producer of both
    /// the history and the tail. Committing the pending batch first is what
    /// makes the halves a partition: a finalized line that was still only
    /// buffered would be in neither the committed history nor the mutable
    /// tail. Nothing runs between the flush, the read, and the sample, so no
    /// later byte can enter the cut.
    ///
    /// A degraded sink refuses instead of answering: its persistence is
    /// broken, so a cut of it could no longer be guaranteed to be a
    /// partition. A batch the writer had to *drop* is different: its range
    /// was recorded as an explicit gap, its numbers are consumed, and the
    /// returned page reports `degraded`, so those lines are declared missing
    /// rather than silently absent from the cut.
    async fn checkpoint(
        &mut self,
        normalizer: &mut Normalizer,
        limits: ReadLimits,
    ) -> Result<TailView, String> {
        drain_normalized(
            normalizer,
            &mut self.writer,
            &self.degraded,
            &self.unaccounted,
        )
        .await;
        if self.degraded.load(Ordering::SeqCst) {
            return Err("output sink is degraded; the history is not complete".to_owned());
        }
        match self.writer.flush().await {
            Ok(outcomes) => {
                apply_durability(
                    normalizer,
                    outcomes.normalized,
                    self.writer.last_flush_outcome(LogStream::Normalized),
                    &self.degraded,
                    &self.unaccounted,
                );
                if self.degraded.load(Ordering::SeqCst) {
                    return Err("output sink is degraded; the history is not complete".to_owned());
                }
            }
            Err(error) => {
                self.degraded.store(true, Ordering::SeqCst);
                self.unaccounted.store(true, Ordering::SeqCst);
                return Err(error.to_string());
            }
        }
        // The read mints its fixed end line at the watermark that flush just
        // committed.
        let request = ReadRequest::first(self.log.clone(), Some(ReadStart::Newest), limits)
            .map_err(|error| error.to_string())?;
        let history = self
            .store
            .read(&request)
            .await
            .map_err(|error| error.to_string())?;
        // Sampled in this sink's own order, immediately after the committed
        // history it complements.
        let tail = normalizer.snapshot();
        Ok(TailView::new(history, tail))
    }
}

/// Marks the sink's end as **unverified** unless the sink published a
/// verified finish itself.
///
/// It fires on every exit path, including a panic or an aborted task — and
/// that is exactly the case where the writer's pending batch may never have
/// been closed durably. A failed `close` is published as `Unverified` by
/// [`OutputSink::run`] itself; a panic, an abort, or an early return is
/// downgraded here.
struct SinkFinish(watch::Sender<SinkOutcome>);

impl Drop for SinkFinish {
    fn drop(&mut self) {
        let _ = self.0.send_if_modified(|state| {
            if *state == SinkOutcome::Running {
                *state = SinkOutcome::Unverified;
                true
            } else {
                false
            }
        });
    }
}

/// Serve one consistent cut of `log`'s committed history plus its mutable
/// tail.
///
/// The checkpoint message rides the sink's ordered output queue, so it
/// observes every byte the reader handed over before the request even
/// reached the terminal. When the sink has finished instead of answering,
/// its **published finish state** decides: only a verified durable close
/// (every accepted line committed or recorded as dropped) may be read as a
/// cut. A failed close, a panic, an aborted task, or unaccounted normalized
/// data yields [`RuntimeError::Storage`] instead — a supposedly consistent
/// fallback is never served from an unverified state. `OutputState::Closed`
/// is deliberately not used for this, because the reader commits it before
/// the sink has flushed.
///
/// Cancellation needs no cleanup: this function holds no lock and pauses no
/// task, so dropping it simply drops the reply channel.
async fn sink_tail_cut(
    output_end_tx: &mpsc::UnboundedSender<OutputMessage>,
    sink_done: &watch::Receiver<SinkOutcome>,
    store: &LogStore,
    log: &LogIdentity,
    tail: &Arc<Mutex<TailState>>,
    limits: ReadLimits,
) -> Result<TailView, RuntimeError> {
    let (response, receiver) = oneshot::channel();
    if output_end_tx
        .send(OutputMessage::Checkpoint { limits, response })
        .is_ok()
    {
        let mut done = sink_done.clone();
        let outcome = tokio::select! {
            reply = receiver => match reply {
                Ok(Ok(view)) => return Ok(view),
                Ok(Err(detail)) => return Err(RuntimeError::Storage { detail }),
                // The reply was dropped, which only the sink task's end
                // does: its published finish state is the authority.
                Err(_) => *done.borrow(),
            },
            outcome = sink_finished(&mut done) => outcome,
        };
        require_durable_finish(outcome)?;
    } else {
        // The sink is already gone; only its published finish state can be
        // trusted.
        require_durable_finish(*sink_done.borrow())?;
    }
    // The sink finished durably: its writer closed (so every accepted line
    // is durable or explicitly dropped) and it is the only mutator of the
    // tail, which makes a plain read plus snapshot a consistent cut.
    let request =
        ReadRequest::first(log.clone(), Some(ReadStart::Newest), limits).map_err(query_error)?;
    let history = store.read(&request).await.map_err(storage_error)?;
    let snapshot = tail.lock().expect("tail state").snapshot();
    Ok(TailView::new(history, snapshot))
}

/// Only a verified, durable finish may be read as a consistent cut.
fn require_durable_finish(outcome: SinkOutcome) -> Result<(), RuntimeError> {
    if outcome == SinkOutcome::Durable {
        return Ok(());
    }
    Err(RuntimeError::Storage {
        detail: "output sink ended without a verified durable close; the frozen \
                 history is not a verified cut"
            .to_owned(),
    })
}

/// Wait until the sink has published how it ended. A `Running` result means
/// every sender is gone without a publication, which no path in this crate
/// does; it is reported as-is so the caller refuses rather than falls back.
async fn sink_finished(done: &mut watch::Receiver<SinkOutcome>) -> SinkOutcome {
    loop {
        let current = *done.borrow_and_update();
        if current != SinkOutcome::Running {
            return current;
        }
        if done.changed().await.is_err() {
            return *done.borrow();
        }
    }
}

/// Persist everything the normalizer produced, in order: each finalized line
/// keeps its stable number, each retired number is recorded as an explicit
/// loss, and a line's tail mapping is published **only when a durability
/// proof arrives** — never merely because the writer accepted the line.
///
/// Durability routes handled here:
///
/// - a size-bound append reports [`AppendOutcome::Committed`], which makes
///   the whole batch (this line and every buffered line before it) durable;
/// - an append that is still [`AppendOutcome::Buffered`] proves nothing, so
///   the mapping stays private until a flush or a commit proves it;
/// - [`AppendOutcome::Dropped`] means the batch was recorded as an explicit
///   gap: its lines do not exist, so its mappings expire;
/// - recording a loss flushes the pending batch first, so the writer's most
///   recent batch-bearing outcome is the proof for the waiting mappings.
///
/// A failure (or a line skipped because the sink already gave up) is
/// *unaccounted*: the normalized stream produced a line that was neither
/// committed nor recorded as dropped, which later forbids a frozen-state
/// fallback.
async fn drain_normalized(
    normalizer: &mut Normalizer,
    writer: &mut LogWriter,
    degraded: &AtomicBool,
    unaccounted: &AtomicBool,
) {
    while let Some(item) = normalizer.next_pending() {
        if degraded.load(Ordering::SeqCst) {
            // Persistence already failed: this accepted line is neither
            // committed nor recorded as dropped, so its mapping stays
            // private and the frozen state can never be served as a cut.
            unaccounted.store(true, Ordering::SeqCst);
            continue;
        }
        match item {
            PendingLine::Line { line, text } => match writer.append_line(line, &text).await {
                Ok(appended) => match appended.outcome {
                    // The batch bound was reached: everything accepted so
                    // far is durable.
                    AppendOutcome::Committed => normalizer.mappings_committed(),
                    // Not durable yet: the mapping stays private.
                    AppendOutcome::Buffered => {}
                    // Durably dropped as an explicit gap: the lines do not
                    // exist, so their mappings expire.
                    AppendOutcome::Dropped => {
                        degraded.store(true, Ordering::SeqCst);
                        normalizer.mappings_dropped();
                    }
                },
                Err(_) => {
                    degraded.store(true, Ordering::SeqCst);
                    unaccounted.store(true, Ordering::SeqCst);
                }
            },
            PendingLine::Loss { first_line, lines } => {
                match writer.record_line_loss(first_line, lines).await {
                    Ok(_) => {
                        // Recording a loss commits the pending batch on its
                        // way, so the writer's most recent batch-bearing
                        // outcome is the durability proof.
                        apply_durability(
                            normalizer,
                            StreamFlushOutcome::Nothing,
                            writer.last_flush_outcome(LogStream::Normalized),
                            degraded,
                            unaccounted,
                        );
                    }
                    Err(_) => {
                        degraded.store(true, Ordering::SeqCst);
                        unaccounted.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// Turn an observed normalized-stream outcome into the mapping state it
/// proves.
///
/// `observed` is the outcome of the batch-bearing operation just performed;
/// [`StreamFlushOutcome::Nothing`] means that operation had no batch of its
/// own, so the batch containing the waiting mappings was ended earlier (the
/// 50 ms deadline driver, a size-bound append, or a previous close) and
/// `last_batch` — the writer's most recent batch-bearing outcome — is the
/// authority.
fn apply_durability(
    normalizer: &mut Normalizer,
    observed: StreamFlushOutcome,
    last_batch: StreamFlushOutcome,
    degraded: &AtomicBool,
    unaccounted: &AtomicBool,
) {
    let settled = if observed == StreamFlushOutcome::Nothing {
        last_batch
    } else {
        observed
    };
    match settled {
        StreamFlushOutcome::Committed => normalizer.mappings_committed(),
        StreamFlushOutcome::Dropped => {
            degraded.store(true, Ordering::SeqCst);
            normalizer.mappings_dropped();
        }
        // No batch-bearing flush was ever recorded for the normalized stream:
        // that proves nothing, so nothing is published. A mapping is then
        // waiting for a proof that cannot come, which makes its line
        // unaccounted for — but only when something really is waiting (a
        // loss recorded before any line was accepted has none).
        StreamFlushOutcome::Nothing => {
            if normalizer.has_pending_mappings() {
                unaccounted.store(true, Ordering::SeqCst);
            }
        }
    }
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

#[cfg(test)]
mod pipeline_tests {
    //! End-to-end tests of the normalization pipeline into the storage
    //! seam: bytes are fed exactly as the reader would hand them over, the
    //! sink drains them into a real `LogWriter`, and the result is queried
    //! through the public storage surface. They need no PTY and no cgroup,
    //! so the normalization contract stays verified even where cgroup
    //! delegation is unavailable.

    use super::*;
    use qingluan_core::terminal::{
        ExternalSessionId, GrepLimits, GrepQuery, GrepRequest, HistoryPosition, LogEpoch,
        QueryError, ReadLimits, ReadRequest, ReadStart, SessionRef, SessionSource, TerminalId,
    };
    use qingluan_storage::{LogStore, StorageError};

    use crate::limits::TAIL_MAX_BYTES;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ql-terminal-s4-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempRoot(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn identity() -> LogIdentity {
        LogIdentity {
            terminal: TerminalRef {
                session: SessionRef {
                    source: SessionSource::new("test"),
                    external_id: ExternalSessionId::new("s1"),
                },
                terminal_id: TerminalId::new(uuid::Uuid::now_v7().to_string()),
            },
            log_epoch: LogEpoch::new(uuid::Uuid::now_v7().to_string()),
        }
    }

    fn pos(line: u64, byte_offset: u64) -> HistoryPosition {
        HistoryPosition::new(line, byte_offset).expect("valid position")
    }

    fn start_at(line: u64) -> Option<ReadStart> {
        Some(ReadStart::At(pos(line, 0)))
    }

    /// The outcome of running the normalization pipeline over a byte
    /// stream: the tail state after the end of output, and the history
    /// position the last mutable revision resolved to (if it resolved).
    struct Outcome {
        tail: Arc<Mutex<TailState>>,
        resolved: Option<HistoryPosition>,
    }

    /// Run the sink's normalization over `chunks`, then the end of output,
    /// against a real writer of `store`. Durability is reconciled exactly the
    /// way the sink does it, so the mapping assertions are about the
    /// production rule and not about the helper.
    async fn normalize(store: &LogStore, log: &LogIdentity, chunks: &[&[u8]]) -> Outcome {
        let mut writer = store.open_writer(log).await.expect("writer");
        let tail = Arc::new(Mutex::new(TailState::new(
            log.clone(),
            TailId::new("pipeline-tail"),
            writer.line_watermark() + 1,
        )));
        let degraded = Arc::new(AtomicBool::new(false));
        let unaccounted = Arc::new(AtomicBool::new(false));
        let mut normalizer = Normalizer::new(Arc::clone(&tail));
        let mut last = None;
        for chunk in chunks {
            normalizer.feed(chunk);
            drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
            last = Some(tail.lock().unwrap().snapshot().position().clone());
        }
        normalizer.finish();
        drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
        let before_close = last
            .as_ref()
            .and_then(|position| tail.lock().unwrap().resolve(position));
        let closed = writer.close().await.expect("close");
        // The close is the durability proof for the final buffered batch.
        apply_durability(
            &mut normalizer,
            closed.normalized,
            StreamFlushOutcome::Nothing,
            &degraded,
            &unaccounted,
        );
        let resolved = last
            .as_ref()
            .and_then(|position| tail.lock().unwrap().resolve(position));
        assert!(!degraded.load(Ordering::SeqCst), "storage stayed healthy");
        assert!(
            !unaccounted.load(Ordering::SeqCst),
            "storage stayed healthy"
        );
        if before_close.is_some() {
            assert_eq!(
                before_close, resolved,
                "a mapping that resolved before the close proves nothing new"
            );
        }
        Outcome { tail, resolved }
    }

    /// Read every committed history line, collapsing a line that spans
    /// fragments. A leading explicit loss is skipped by restarting at the
    /// earliest readable position the refusal reports.
    async fn history(store: &LogStore, log: &LogIdentity) -> Vec<(u64, String)> {
        let mut request =
            ReadRequest::first(log.clone(), Some(ReadStart::Earliest), ReadLimits::DEFAULT)
                .expect("request");
        let mut out: Vec<(u64, String)> = Vec::new();
        loop {
            let result = match store.read(&request).await {
                Ok(result) => result,
                Err(StorageError::Query(QueryError::CursorExpired { earliest, .. })) => {
                    let start = earliest.expect("an earliest readable position");
                    request = ReadRequest::first(
                        log.clone(),
                        Some(ReadStart::At(start)),
                        ReadLimits::DEFAULT,
                    )
                    .expect("request");
                    continue;
                }
                Err(other) => panic!("read: {other}"),
            };
            for fragment in result.page().fragments() {
                match out.last_mut() {
                    Some((line, text)) if *line == fragment.position().line() => {
                        text.push_str(fragment.text());
                    }
                    _ => out.push((fragment.position().line(), fragment.text().to_owned())),
                }
            }
            match result.page().next() {
                Some(next) => request = ReadRequest::resume(next.clone(), ReadLimits::DEFAULT),
                None => return out,
            }
        }
    }

    /// Drives the real output sink exactly the way the PTY reader does
    /// (bounded-handoff messages plus an end), and asks it for tail cuts
    /// through the same function the runtime uses. No PTY and no cgroup, so
    /// the ordering contract is testable deterministically.
    struct Sink {
        store: LogStore,
        log: LogIdentity,
        tx: mpsc::UnboundedSender<OutputMessage>,
        queued: Arc<AtomicU64>,
        done: watch::Receiver<SinkOutcome>,
        degraded: Arc<AtomicBool>,
        unaccounted: Arc<AtomicBool>,
        tail: Arc<Mutex<TailState>>,
        handle: Option<JoinHandle<Result<(), String>>>,
    }

    impl Sink {
        async fn new(store: &LogStore, log: &LogIdentity) -> Sink {
            let writer = store.open_writer(log).await.expect("writer");
            let tail = Arc::new(Mutex::new(TailState::new(
                log.clone(),
                TailId::new("sink-harness"),
                writer.line_watermark() + 1,
            )));
            let (tx, rx) = mpsc::unbounded_channel::<OutputMessage>();
            let (done_tx, done) = watch::channel(SinkOutcome::Running);
            let queued = Arc::new(AtomicU64::new(0));
            let degraded = Arc::new(AtomicBool::new(false));
            let unaccounted = Arc::new(AtomicBool::new(false));
            let sink = OutputSink {
                writer,
                store: store.clone(),
                log: log.clone(),
                queued: Arc::clone(&queued),
                degraded: Arc::clone(&degraded),
                unaccounted: Arc::clone(&unaccounted),
                tail: Arc::clone(&tail),
                done: done_tx,
            };
            let handle = tokio::spawn(async move { sink.run(rx).await });
            Sink {
                store: store.clone(),
                log: log.clone(),
                tx,
                queued,
                done,
                degraded,
                unaccounted,
                tail,
                handle: Some(handle),
            }
        }

        /// Hand bytes over exactly as the reader's bounded handoff does.
        fn bytes(&self, bytes: &[u8]) {
            self.queued.fetch_add(bytes.len() as u64, Ordering::SeqCst);
            self.tx
                .send(OutputMessage::Bytes {
                    gap_before: 0,
                    bytes: bytes.to_vec(),
                })
                .expect("sink is running");
        }

        /// One consistent cut, requested through the runtime's own path.
        async fn cut(&self, limits: ReadLimits) -> Result<TailView, RuntimeError> {
            sink_tail_cut(
                &self.tx,
                &self.done,
                &self.store,
                &self.log,
                &self.tail,
                limits,
            )
            .await
        }

        /// Ask for a cut and drop the reply, as a cancelled caller would.
        fn send_unwaited_checkpoint(&self, limits: ReadLimits) {
            let (response, receiver) = oneshot::channel();
            self.tx
                .send(OutputMessage::Checkpoint { limits, response })
                .expect("sink is running");
            drop(receiver);
        }

        fn end(&self) {
            self.tx
                .send(OutputMessage::End { trailing_gap: 0 })
                .expect("sink is running");
        }

        /// Join the sink task (it ends after the output end, once its writer
        /// is closed).
        async fn await_end(&mut self) {
            if let Some(handle) = self.handle.take() {
                handle.await.expect("sink join").expect("sink result");
            }
        }

        fn healthy(&self) -> bool {
            !self.degraded.load(Ordering::SeqCst)
        }

        /// How the sink has published its end (or `Running`).
        fn outcome(&self) -> SinkOutcome {
            *self.done.borrow()
        }

        /// Wait until the sink has processed everything handed over so far.
        /// The handoff counter reaches zero only after the sink has fed the
        /// bytes to the normalizer *and* drained what they produced, which is
        /// the synchronization point these tests need.
        async fn await_processed(&self) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while self.queued.load(Ordering::SeqCst) != 0 {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the sink did not drain the handed-over bytes"
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    /// The lines one cut reports as committed history, in order.
    fn cut_lines(cut: &TailView) -> Vec<(u64, String)> {
        cut.history()
            .page()
            .fragments()
            .iter()
            .map(|fragment| (fragment.position().line(), fragment.text().to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn normalized_lines_reach_storage_and_are_queryable() {
        let root = TempRoot::new("pipeline");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();

        // Chunks are deliberately split inside control sequences, inside
        // multi-byte characters, and inside escape payloads.
        let chunks: Vec<&[u8]> = vec![
            "你好".as_bytes(),
            b"\r",
            b"X",
            b"\n",
            "e\u{301}".as_bytes(),
            "t\u{4e16}a\tb\n".as_bytes(),
            b"\x1b[1;",
            b"31m",
            b"styled\x1b[0m",
            b"\n\x1b]0;title",
            b"\x07",
            b"after-osc\n",
            b"no newline",
        ];
        let outcome = normalize(&store, &log, &chunks).await;

        // The golden normalized history: CR overwrites in place, style and
        // OSC are discarded, the combining mark stays attached to its base,
        // the tab is padded to the next stop, and CJK is preserved.
        let lines = history(&store, &log).await;
        assert_eq!(
            lines,
            vec![
                (1, "X好".to_owned()),
                (2, "e\u{301}t\u{4e16}a   b".to_owned()),
                (3, "styled".to_owned()),
                (4, "after-osc".to_owned()),
                (5, "no newline".to_owned()),
            ]
        );

        // The mutable tail is empty once the end of output finalized it, and
        // the last mutable revision resolves onto the line it became.
        assert!(outcome.tail.lock().unwrap().snapshot().text().is_empty());
        assert_eq!(
            outcome
                .resolved
                .expect("the finalized tail resolves as history"),
            pos(5, 0)
        );

        // A literal grep over the committed history finds the CJK line, with
        // its bounded context lines.
        let query = GrepQuery::new(log.clone(), "t世", true, pos(1, 0), 4, 1).expect("query");
        let page = store
            .grep(&GrepRequest::fresh(query), GrepLimits::DEFAULT)
            .await
            .expect("grep");
        assert_eq!(page.matches().len(), 1);
        assert_eq!(page.matches()[0].position().line(), 2);
        // "e" + combining U+0301 occupy three bytes, so the match starts
        // there, not at the CJK character.
        assert_eq!(page.matches()[0].position().byte_offset(), 3);
        assert_eq!(
            page.contexts().iter().map(|c| c.line()).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[tokio::test]
    async fn an_over_long_mutable_line_is_bounded_and_recorded_as_an_explicit_gap() {
        let root = TempRoot::new("overlong");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();

        let long = vec![b'l'; TAIL_MAX_BYTES + 4096];
        let outcome = normalize(&store, &log, &[&long]).await;
        assert!(outcome.tail.lock().unwrap().snapshot().text().is_empty());
        // The snapshot's own position — the first byte the bounded tail
        // still retained — resolves to the start of the suffix line; an
        // offset inside the omitted prefix does not (asserted by the
        // normalizer's own unit tests).
        assert_eq!(outcome.resolved, Some(pos(2, 0)));

        // Line 1 is an explicit loss: asking for it is a typed refusal that
        // reports the earliest readable position and the missing range, not
        // a silently short line.
        match store
            .read(
                &ReadRequest::first(log.clone(), start_at(1), ReadLimits::DEFAULT)
                    .expect("request"),
            )
            .await
        {
            Err(StorageError::Query(QueryError::CursorExpired { earliest, missing })) => {
                assert_eq!(earliest.map(|position| position.line()), Some(2));
                assert!(missing.is_some(), "the missing range is reported");
            }
            other => panic!("expected a typed expiry, got {other:?}"),
        }

        // The retained suffix is readable as line 2, is bounded, and the
        // page reports the terminal's explicit loss.
        let lines = history(&store, &log).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, 2);
        assert!(!lines[0].1.is_empty());
        assert!(lines[0].1.len() <= TAIL_MAX_BYTES);
        assert!(lines[0].1.bytes().all(|byte| byte == b'l'));
        let page = store
            .read(
                &ReadRequest::first(log.clone(), start_at(2), ReadLimits::DEFAULT)
                    .expect("request"),
            )
            .await
            .expect("read");
        assert!(page.page().degraded());
    }

    #[tokio::test]
    async fn the_output_sink_persists_lines_and_finalizes_the_tail_at_the_end() {
        let root = TempRoot::new("sink");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        // The reader's messages, including a read split inside a multi-byte
        // character.
        sink.bytes("你".as_bytes());
        sink.bytes("好\n".as_bytes());
        sink.bytes(b"tail");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "你好".to_owned())]);
        assert_eq!(cut.tail().text(), "tail");
        assert!(sink.healthy());

        sink.end();
        sink.await_end().await;
        assert!(sink.healthy());

        assert_eq!(
            history(&store, &log).await,
            vec![(1, "你好".to_owned()), (2, "tail".to_owned())],
            "the unfinished tail is one history line at the end of output"
        );
    }

    #[tokio::test]
    async fn the_output_sink_records_a_handoff_gap_without_fabricating_lines() {
        let root = TempRoot::new("sink-gap");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        sink.queued
            .fetch_add(4096, std::sync::atomic::Ordering::SeqCst);
        sink.tx
            .send(OutputMessage::Bytes {
                gap_before: 4096,
                bytes: b"after\n".to_vec(),
            })
            .expect("sink is running");
        sink.end();
        sink.await_end().await;

        // The real bytes are committed under their own numbers; the unknown
        // loss is the terminal's explicit degraded latch, and the query
        // surface reports it instead of presenting continuous history.
        assert_eq!(history(&store, &log).await, vec![(1, "after".to_owned())]);
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "after".to_owned())]);
        assert!(
            cut.history().page().degraded(),
            "the loss is reported, never hidden"
        );
        assert!(sink.healthy(), "the normalized stream itself is intact");
    }

    #[tokio::test]
    async fn a_discontinuity_commits_the_real_bytes_and_keeps_numbering_monotonic() {
        let root = TempRoot::new("discontinuity");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut writer = store.open_writer(&log).await.expect("writer");
        let tail = Arc::new(Mutex::new(TailState::new(
            log.clone(),
            TailId::new("discontinuity-tail"),
            writer.line_watermark() + 1,
        )));
        let degraded = Arc::new(AtomicBool::new(false));
        let mut normalizer = Normalizer::new(Arc::clone(&tail));

        normalizer.feed(b"before");
        let unaccounted = AtomicBool::new(false);
        drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
        // The bounded handoff dropped an unknown run: the real pending bytes
        // are committed, and no normalized line count is invented for what
        // was dropped.
        normalizer.discontinuity();
        drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
        normalizer.feed(b"after\n");
        drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
        normalizer.finish();
        drain_normalized(&mut normalizer, &mut writer, &degraded, &unaccounted).await;
        writer.close().await.expect("close");
        assert!(!degraded.load(Ordering::SeqCst));

        assert_eq!(
            history(&store, &log).await,
            vec![(1, "before".to_owned()), (2, "after".to_owned())],
            "numbering is monotonic and no line is lost"
        );
    }

    #[tokio::test]
    async fn blank_lines_persist_as_their_own_history_lines() {
        let root = TempRoot::new("blank-lines");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        sink.bytes(b"\n\ntext\n");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![
                (1, String::new()),
                (2, String::new()),
                (3, "text".to_owned()),
            ],
            "each separator fixes its own history line"
        );
        assert!(cut.tail().text().is_empty(), "nothing is pending");

        // A separator with no content still consumes its number, and the
        // unterminated tail stays mutable.
        sink.bytes(b"\nmore");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![
                (1, String::new()),
                (2, String::new()),
                (3, "text".to_owned()),
                (4, String::new()),
            ]
        );
        assert_eq!(cut.tail().text(), "more");
        assert!(!cut.tail().truncated());

        // The end of output fixes the mutable tail exactly once and does not
        // fabricate an extra empty line behind the trailing separator.
        sink.end();
        sink.await_end().await;
        assert_eq!(
            history(&store, &log).await,
            vec![
                (1, String::new()),
                (2, String::new()),
                (3, "text".to_owned()),
                (4, String::new()),
                (5, "more".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_tail_cut_commits_a_buffered_line_and_never_misses_or_duplicates_it() {
        let root = TempRoot::new("buffered-cut");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        // "first" is finalized (LF) but the writer may still hold it in its
        // pending batch, while "second" is still the mutable tail. A cut
        // that sampled the two halves independently would drop "first" out
        // of both.
        sink.bytes(b"first\nsecond");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "first".to_owned())]);
        assert_eq!(cut.tail().text(), "second");
        assert_eq!(cut.history().cursor().end_line(), 1);
        assert!(sink.healthy());

        // Later output: the committed line never reappears, and the new line
        // appears in exactly one half.
        sink.bytes(b"\nthird");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![(1, "first".to_owned()), (2, "second".to_owned())]
        );
        assert_eq!(cut.tail().text(), "third");
        assert_eq!(cut.history().cursor().end_line(), 2);

        sink.end();
        sink.await_end().await;
        assert_eq!(
            history(&store, &log).await,
            vec![
                (1, "first".to_owned()),
                (2, "second".to_owned()),
                (3, "third".to_owned()),
            ],
            "every line is persisted exactly once"
        );
    }

    #[tokio::test]
    async fn a_newline_at_the_cut_lands_in_exactly_one_half() {
        let root = TempRoot::new("boundary-cut");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        // The separator was queued before the checkpoint: it belongs to the
        // committed half and the tail is empty.
        sink.bytes(b"a\n");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "a".to_owned())]);
        assert!(cut.tail().text().is_empty());

        // A partial line is queued after the checkpoint: it is not part of
        // this cut at all.
        sink.bytes(b"b");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "a".to_owned())]);
        assert_eq!(cut.tail().text(), "b");

        // The separator now lands exactly on a cut boundary.
        sink.bytes(b"\n");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![(1, "a".to_owned()), (2, "b".to_owned())]
        );
        assert!(cut.tail().text().is_empty());
        assert_eq!(cut.history().cursor().end_line(), 2);

        sink.end();
        sink.await_end().await;
        assert_eq!(
            history(&store, &log).await,
            vec![(1, "a".to_owned()), (2, "b".to_owned())],
            "the boundary line appears exactly once"
        );
    }

    #[tokio::test]
    async fn a_tail_cut_keeps_its_fixed_range_after_later_output() {
        let root = TempRoot::new("cut-fixed-range");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let sink = Sink::new(&store, &log).await;
        let small = ReadLimits::new(2, 32 * 1024).unwrap();

        sink.bytes(b"one\ntwo\nthree\n");
        let cut = sink.cut(small).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![(2, "two".to_owned()), (3, "three".to_owned())],
            "the newest window fits the line budget"
        );
        assert_eq!(cut.history().cursor().end_line(), 3);
        assert!(cut.tail().text().is_empty());

        // Later output never extends the fixed range of that cursor.
        sink.bytes(b"four\nfive\nsix\n");
        let resumed = store
            .read(&ReadRequest::resume(cut.history().cursor().clone(), small))
            .await
            .expect("read");
        let lines: Vec<u64> = resumed
            .page()
            .fragments()
            .iter()
            .map(|fragment| fragment.position().line())
            .collect();
        assert_eq!(lines, vec![2, 3]);

        // A fresh cut sees the new lines, exactly once each.
        let cut = sink.cut(small).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![(5, "five".to_owned()), (6, "six".to_owned())]
        );
        assert!(cut.tail().text().is_empty());
        assert!(sink.healthy());
    }

    #[tokio::test]
    async fn a_cancelled_tail_wait_cannot_leave_the_sink_paused() {
        let root = TempRoot::new("cancelled-cut");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let sink = Sink::new(&store, &log).await;

        // Nobody waits for this checkpoint's reply: the sink must still serve
        // it and keep draining.
        sink.send_unwaited_checkpoint(ReadLimits::DEFAULT);

        // A runtime-style request that is cancelled mid-flight must not pause
        // the sink either.
        let tx = sink.tx.clone();
        let done = sink.done.clone();
        let store_handle = store.clone();
        let log_handle = log.clone();
        let tail = Arc::clone(&sink.tail);
        let cancelled = tokio::spawn(async move {
            sink_tail_cut(
                &tx,
                &done,
                &store_handle,
                &log_handle,
                &tail,
                ReadLimits::DEFAULT,
            )
            .await
        });
        tokio::task::yield_now().await;
        cancelled.abort();
        let _ = cancelled.await;

        // The next awaited cut is consistent and the sink is still healthy.
        sink.bytes(b"one\ntwo");
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "one".to_owned())]);
        assert_eq!(cut.tail().text(), "two");
        assert!(sink.healthy());
    }

    #[tokio::test]
    async fn a_finished_sink_still_serves_a_consistent_cut() {
        let root = TempRoot::new("finished-cut");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        // The end is queued before the request: the sink closes and flushes
        // whichever order the two are dequeued in, so the cut must still be
        // the frozen, fully committed state.
        sink.bytes(b"x");
        sink.await_processed().await;
        let position = sink.tail.lock().unwrap().snapshot().position().clone();
        sink.bytes(b"\nunfinished");
        sink.end();
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(
            cut_lines(&cut),
            vec![(1, "x".to_owned()), (2, "unfinished".to_owned())],
            "the end of output fixed the unfinished tail"
        );
        assert!(cut.tail().text().is_empty());
        sink.await_end().await;

        // A verified durable close is what the frozen state needs, and it is
        // also the proof that publishes the already-buffered lines: "x" was
        // only accepted (Buffered) until the close drained it.
        assert_eq!(sink.outcome(), SinkOutcome::Durable);
        assert_eq!(
            sink.tail.lock().unwrap().resolve(&position),
            Some(pos(1, 0)),
            "the close proves durability for the buffered line"
        );
        let fallback = sink.cut(ReadLimits::DEFAULT).await.expect("fallback");
        assert_eq!(fallback_lines(&fallback), cut_lines(&cut));
    }

    /// The lines one frozen-state (post-finish) cut reports, in order.
    fn fallback_lines(cut: &TailView) -> Vec<(u64, String)> {
        cut_lines(cut)
    }

    #[tokio::test]
    async fn a_buffered_mapping_cannot_resolve_until_the_batch_is_durable() {
        let root = TempRoot::new("buffered-mapping");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let sink = Sink::new(&store, &log).await;

        // "first" is the mutable tail; a reader holds this exact revision.
        sink.bytes(b"first");
        sink.await_processed().await;
        let position = sink.tail.lock().unwrap().snapshot().position().clone();

        // The separator finalizes it. The writer accepts the line but only
        // *buffers* it (the 64 KiB/50 ms boundary has not been reached), so
        // the old tail position must not resolve to a history line yet: the
        // line is not readable.
        sink.bytes(b"\n");
        sink.await_processed().await;
        assert!(
            sink.tail.lock().unwrap().resolve(&position).is_none(),
            "a Buffered line must not resolve"
        );

        // The checkpoint flushes, which is the durability proof.
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "first".to_owned())]);
        assert_eq!(
            sink.tail.lock().unwrap().resolve(&position),
            Some(pos(1, 0)),
            "a proven line resolves to its stable history line"
        );
    }

    #[tokio::test]
    async fn a_driver_committed_batch_resolves_at_the_next_proof_point() {
        let root = TempRoot::new("driver-mapping");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        sink.bytes(b"driven");
        sink.await_processed().await;
        let position = sink.tail.lock().unwrap().snapshot().position().clone();
        sink.bytes(b"\n");
        sink.await_processed().await;

        // The 50 ms deadline driver commits the batch on its own; the sink
        // never observes that outcome, so publication waits for the next
        // proof point instead of guessing.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            sink.tail.lock().unwrap().resolve(&position).is_none(),
            "an unobserved driver commit is not a proof the sink can use"
        );

        // The checkpoint's flush finds nothing pending and consults the
        // writer's most recent batch-bearing outcome, which is Committed.
        let cut = sink.cut(ReadLimits::DEFAULT).await.expect("cut");
        assert_eq!(cut_lines(&cut), vec![(1, "driven".to_owned())]);
        assert_eq!(
            sink.tail.lock().unwrap().resolve(&position),
            Some(pos(1, 0))
        );

        sink.end();
        sink.await_end().await;
        assert_eq!(sink.outcome(), SinkOutcome::Durable);
    }

    #[tokio::test]
    async fn a_size_bound_append_proves_durability_without_a_checkpoint() {
        let root = TempRoot::new("size-bound-mapping");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        sink.bytes(b"first");
        sink.await_processed().await;
        let position = sink.tail.lock().unwrap().snapshot().position().clone();
        sink.bytes(b"\n");
        sink.await_processed().await;
        assert!(sink.tail.lock().unwrap().resolve(&position).is_none());

        // Push the pending batch past the 64 KiB bound: the append that
        // reaches it flushes synchronously and reports Committed, which is
        // the durability proof — no checkpoint involved.
        let line = format!("{}\n", "y".repeat(7000));
        for _ in 0..20 {
            sink.bytes(line.as_bytes());
        }
        sink.await_processed().await;
        assert_eq!(
            sink.tail.lock().unwrap().resolve(&position),
            Some(pos(1, 0)),
            "a size-bound commit proves durability"
        );

        sink.end();
        sink.await_end().await;
        assert_eq!(sink.outcome(), SinkOutcome::Durable);
    }

    #[tokio::test]
    async fn an_unaccounted_loss_forbids_a_verified_finish() {
        let root = TempRoot::new("unaccounted-finish");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        let mut sink = Sink::new(&store, &log).await;

        sink.bytes(b"one\n");
        sink.await_processed().await;

        // Persistence gives up before the next line: the writer refused, so
        // the sink stops appending. The next accepted line is then neither
        // committed nor recorded as a dropped gap.
        sink.degraded.store(true, Ordering::SeqCst);
        sink.bytes(b"two");
        sink.await_processed().await;
        let position = sink.tail.lock().unwrap().snapshot().position().clone();
        sink.bytes(b"\n");
        sink.await_processed().await;
        assert!(
            sink.tail.lock().unwrap().resolve(&position).is_none(),
            "a line that was never persisted must not resolve"
        );

        // A live cut is refused...
        assert!(matches!(
            sink.cut(ReadLimits::DEFAULT).await,
            Err(RuntimeError::Storage { .. })
        ));

        // ... and so is the frozen fallback after the end: a clean close
        // cannot vouch for data that was never accounted for.
        sink.end();
        sink.await_end().await;
        assert_eq!(sink.outcome(), SinkOutcome::Unverified);
        assert!(
            matches!(
                sink.cut(ReadLimits::DEFAULT).await,
                Err(RuntimeError::Storage { .. })
            ),
            "a supposedly consistent fallback is never served from an unverified sink"
        );
        assert!(sink.unaccounted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn an_unverified_finish_is_refused_instead_of_falling_back() {
        let root = TempRoot::new("unverified-finish");
        let store = LogStore::open(&root.0).await.unwrap();
        let log = identity();
        {
            // Create the terminal row and one committed line, so a fallback
            // *would* return content if it were (wrongly) allowed.
            let mut writer = store.open_writer(&log).await.expect("writer");
            writer.append_line(1, "committed").await.expect("append");
            let outcome = writer.close().await.expect("close");
            assert_eq!(outcome.normalized, StreamFlushOutcome::Committed);
        }
        let tail = Arc::new(Mutex::new(TailState::new(
            log.clone(),
            TailId::new("fake-tail"),
            2,
        )));
        // The sink is gone, so the request cannot be answered and the
        // published finish state decides.
        let (tx, rx) = mpsc::unbounded_channel::<OutputMessage>();
        drop(rx);

        for (outcome, allowed) in [
            (SinkOutcome::Durable, true),
            (SinkOutcome::Unverified, false),
            (SinkOutcome::Running, false),
        ] {
            let (_sender, done) = watch::channel(outcome);
            let result = sink_tail_cut(&tx, &done, &store, &log, &tail, ReadLimits::DEFAULT).await;
            match (allowed, result) {
                (true, Ok(view)) => {
                    assert_eq!(cut_lines(&view), vec![(1, "committed".to_owned())]);
                    assert!(view.tail().text().is_empty());
                }
                (false, Err(RuntimeError::Storage { .. })) => {}
                (_, other) => panic!("outcome {outcome:?} gave {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_finish_guard_marks_every_unverified_end() {
        // A verified finish is never downgraded.
        let (sender, receiver) = watch::channel(SinkOutcome::Running);
        let guard = SinkFinish(sender.clone());
        sender.send_replace(SinkOutcome::Durable);
        drop(guard);
        assert_eq!(*receiver.borrow(), SinkOutcome::Durable);

        // An early return (or any other end that did not publish) is
        // unverified.
        let (sender, receiver) = watch::channel(SinkOutcome::Running);
        drop(SinkFinish(sender));
        assert_eq!(*receiver.borrow(), SinkOutcome::Unverified);

        // A panicking sink task still publishes through the guard, which is
        // the case where its writer may never have been closed durably.
        let (sender, receiver) = watch::channel(SinkOutcome::Running);
        let handle = tokio::spawn(async move {
            let _guard = SinkFinish(sender);
            panic!("sink failure");
        });
        assert!(handle.await.is_err(), "the sink task panicked");
        assert_eq!(*receiver.borrow(), SinkOutcome::Unverified);
    }
}
