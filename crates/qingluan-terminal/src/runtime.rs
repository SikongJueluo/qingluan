//! The public `TerminalRuntime` seam.
//!
//! One runtime instance owns a storage root, the durable runtime registry,
//! the manager-owned cgroup root, the two-level activity quota, each
//! session's control generation, and every live terminal. All operations
//! speak in `qingluan_core::terminal` domain types; nothing behind the seam
//! (PTY fds, cgroups, storage, wire shapes) appears in a signature.
//!
//! Startup recovery is deliberately narrow: every unfinished durable record
//! is marked `Interrupted` (never a fabricated exit) through the storage
//! seam, and the manager's own leftover terminal cgroups are reconciled by
//! cgroup identity. A persisted pid is never reattached or signalled.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use qingluan_core::terminal::{
    ControlGeneration, SendReceipt, SessionRef, StartSpec, TerminalId, TerminalRef, TerminalSize,
    TerminalSnapshot,
};
use qingluan_storage::{LogStore, RuntimeRecord, RuntimeRegistry};
use tokio::sync::watch;

use crate::cgroup::DelegatedRoot;
use crate::config::RuntimeConfig;
use crate::error::{RuntimeError, SendError, SendRejection};
use crate::limits::SHUTDOWN_WAIT;
use crate::quota::Quota;
use crate::terminal::{CleanupState, StartRequest, Terminal};

struct RuntimeInner {
    store: LogStore,
    registry: RuntimeRegistry,
    cgroup_root: DelegatedRoot,
    quota: Arc<Quota>,
    sessions: Mutex<HashMap<SessionRef, ControlGeneration>>,
    terminals: Mutex<HashMap<TerminalRef, Arc<Terminal>>>,
    /// Handles of starts that observed shutdown at their registration point
    /// and were deliberately never inserted live. `shutdown` stops them and
    /// waits for their detached cleanup before it sweeps the manager cgroup
    /// root, so a late start is never orphaned.
    draining: Mutex<Vec<Arc<Terminal>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown: AtomicBool,
    /// Serializes publishing the shutdown flag against the in-flight start
    /// counter, so `shutdown` can never miss a start that already passed its
    /// first shutdown check.
    start_gate: Mutex<()>,
    /// Starts between their first shutdown check and their registration
    /// decision. `shutdown` waits for this to reach zero before it collects
    /// terminals.
    in_flight: watch::Sender<usize>,
    /// Test seam: parks one start at its pre-registration point.
    #[cfg(any(test, feature = "test-hooks"))]
    start_park: StartPark,
}

/// Counts a start as in-flight from its first shutdown check through its
/// registration decision. `shutdown` publishes its flag under the same gate
/// and then waits for the count to reach zero, so no start can register
/// after shutdown has collected terminals.
struct InFlightStart {
    inner: Arc<RuntimeInner>,
}

impl InFlightStart {
    /// Increment under the start gate. `None` means shutdown is already
    /// published: the start is refused without touching any resource.
    fn begin(inner: &Arc<RuntimeInner>) -> Option<Self> {
        let _gate = inner.start_gate.lock().expect("start gate");
        if inner.shutdown.load(Ordering::SeqCst) {
            return None;
        }
        let next = *inner.in_flight.borrow() + 1;
        inner.in_flight.send_replace(next);
        Some(Self {
            inner: Arc::clone(inner),
        })
    }
}

impl Drop for InFlightStart {
    fn drop(&mut self) {
        let _gate = self.inner.start_gate.lock().expect("start gate");
        let next = *self.inner.in_flight.borrow() - 1;
        self.inner.in_flight.send_replace(next);
    }
}

/// Test-only park at a start's pre-registration point.
#[cfg(any(test, feature = "test-hooks"))]
struct StartPark {
    armed: AtomicBool,
    reached: watch::Sender<bool>,
    release: watch::Sender<bool>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl StartPark {
    fn new() -> Self {
        let (reached, _) = watch::channel(false);
        let (release, _) = watch::channel(false);
        Self {
            armed: AtomicBool::new(false),
            reached,
            release,
        }
    }

    fn arm(&self) {
        self.reached.send_replace(false);
        self.release.send_replace(false);
        self.armed.store(true, Ordering::SeqCst);
    }

    async fn park(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        self.reached.send_replace(true);
        let mut release = self.release.subscribe();
        let _ = release.wait_for(|released| *released).await;
    }

    async fn wait_reached(&self) {
        let mut reached = self.reached.subscribe();
        let _ = reached.wait_for(|reached| *reached).await;
    }

    fn release(&self) {
        self.release.send_replace(true);
    }
}

/// The single external seam for agent terminal execution.
///
/// Cheap to clone indirectly: the runtime is shared behind an `Arc`, and
/// every method takes `&self`.
#[derive(Clone)]
pub struct TerminalRuntime {
    inner: Arc<RuntimeInner>,
}

impl TerminalRuntime {
    /// Open (creating if needed) the storage root, apply migrations, mark
    /// every unfinished durable record `Interrupted`, reconcile the
    /// manager-owned cgroup root, and discover the delegated subtree.
    ///
    /// Missing cgroup delegation or `cgroup.kill` is a hard error; there is
    /// deliberately no fallback to a `/proc` snapshot.
    pub async fn open(root: &Path, config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let store = LogStore::open(root).await.map_err(storage_error)?;
        // One storage root, one SQLite connection: the runtime registry
        // shares the log store's connection so the writer's flush driver and
        // the registry never contend across two connections.
        let registry = store.runtime_registry();

        // Startup marks records Interrupted only: no signal, no fabricated
        // exit, no attempt to reattach a persisted pid.
        registry
            .interrupt_unfinished()
            .await
            .map_err(storage_error)?;

        // Separately reconcile the manager-owned cgroup identities: kill and
        // remove any leftover terminal cgroups from a crashed run, by cgroup
        // identity, before anything new joins.
        let cgroup_root =
            DelegatedRoot::open_or_create(&config.cgroup_tag).map_err(cgroup_error)?;
        cgroup_root.remove().map_err(cgroup_error)?;
        let cgroup_root =
            DelegatedRoot::open_or_create(&config.cgroup_tag).map_err(cgroup_error)?;

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (in_flight, _in_flight_rx) = watch::channel(0usize);
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                store,
                registry,
                cgroup_root,
                quota: Arc::new(Quota::new(config.session_limit, config.global_limit)),
                sessions: Mutex::new(HashMap::new()),
                terminals: Mutex::new(HashMap::new()),
                draining: Mutex::new(Vec::new()),
                shutdown_tx,
                shutdown: AtomicBool::new(false),
                start_gate: Mutex::new(()),
                in_flight,
                #[cfg(any(test, feature = "test-hooks"))]
                start_park: StartPark::new(),
            }),
        })
    }

    /// Start a terminal in `session`. Success means the PTY was spawned and
    /// the durable record is `running`.
    ///
    /// The supplied control generation must be the session's current one;
    /// a stale generation is refused. The terminal_id and log epoch are
    /// qingluan-minted canonical UUIDs.
    pub async fn start(
        &self,
        session: &SessionRef,
        generation: ControlGeneration,
        spec: StartSpec,
    ) -> Result<TerminalRef, RuntimeError> {
        // Passing this gate counts the start as in-flight: shutdown waits for
        // the count to reach zero before it sweeps, so a start that already
        // passed its first shutdown check can never register (or run its late
        // cleanup) after shutdown has collected terminals.
        let _in_flight = match InFlightStart::begin(&self.inner) {
            Some(guard) => guard,
            None => return Err(RuntimeError::Shutdown),
        };
        let terminal = TerminalRef {
            session: session.clone(),
            terminal_id: TerminalId::new(uuid::Uuid::now_v7().to_string()),
        };

        let current = {
            let mut sessions = self.inner.sessions.lock().expect("sessions");
            *sessions
                .entry(session.clone())
                .or_insert_with(ControlGeneration::first)
        };
        if generation != current {
            return Err(RuntimeError::ControlLost(terminal));
        }

        let handle = Terminal::start(
            &self.inner.store,
            &self.inner.registry,
            &self.inner.quota,
            &self.inner.cgroup_root,
            &self.inner.shutdown_tx,
            StartRequest {
                terminal: terminal.clone(),
                generation,
                spec,
            },
        )
        .await?;

        #[cfg(any(test, feature = "test-hooks"))]
        self.inner.start_park.park().await;

        // Register under the same lock the takeover path uses, so the new
        // terminal's generation always matches the session's current one.
        {
            let sessions = self.inner.sessions.lock().expect("sessions");
            let still_current = sessions
                .get(session)
                .copied()
                .unwrap_or_else(ControlGeneration::first);
            if still_current != generation {
                // The control token was invalidated mid-start: the start is
                // refused and the just-spawned terminal is stopped. Track it
                // until cleanup succeeds so a concurrent shutdown cannot
                // sweep the manager cgroup root out from under that cleanup.
                drop(sessions);
                self.drain_unregistered(Arc::clone(&handle));
                return Err(RuntimeError::ControlLost(handle.reference().clone()));
            }
            if self.inner.shutdown.load(Ordering::SeqCst) {
                // Shutdown has been published and is waiting on this
                // in-flight start. Never insert after the sweep: hand the
                // freshly started handle to the drain set so `shutdown`
                // stops it and awaits its cleanup before it removes the
                // manager cgroup root, and refuse the start.
                drop(sessions);
                self.drain_unregistered(Arc::clone(&handle));
                return Err(RuntimeError::Shutdown);
            }
            self.inner
                .terminals
                .lock()
                .expect("terminals")
                .insert(handle.reference().clone(), Arc::clone(&handle));
        }
        Ok(terminal)
    }

    /// Bounded send to a terminal's PTY master.
    pub async fn send(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
        data: Vec<u8>,
    ) -> Result<SendReceipt, SendError> {
        let handle = self
            .lookup(terminal)
            .ok_or(SendError::Rejected(SendRejection::Unknown))?;
        handle.send(generation, data).await
    }

    /// Apply a window-size change to a terminal.
    pub async fn resize(
        &self,
        terminal: &TerminalRef,
        generation: ControlGeneration,
        size: TerminalSize,
    ) -> Result<(), RuntimeError> {
        let handle = self.require(terminal)?;
        handle.resize(generation, size).await
    }

    /// Commit a stop and return the current snapshot immediately. The
    /// cleanup continues in a detached task; repeated calls share it.
    pub async fn stop(&self, terminal: &TerminalRef) -> Result<TerminalSnapshot, RuntimeError> {
        let handle = self.require_snapshot(terminal).await?;
        if let Some(handle) = handle {
            handle.stop();
            Ok(handle.snapshot())
        } else {
            // A durable record with no live handle: return its recorded
            // state (already ended, or left by a previous run).
            self.snapshot(terminal).await
        }
    }

    /// The current snapshot of one terminal.
    pub async fn snapshot(&self, terminal: &TerminalRef) -> Result<TerminalSnapshot, RuntimeError> {
        if let Some(handle) = self.lookup(terminal) {
            return Ok(handle.snapshot());
        }
        match self
            .inner
            .registry
            .load(terminal)
            .await
            .map_err(storage_error)?
        {
            Some(record) => Ok(record_snapshot(record)),
            None => Err(RuntimeError::UnknownTerminal(terminal.clone())),
        }
    }

    /// Every known terminal, in the registry's stable order.
    pub async fn list(&self) -> Result<Vec<TerminalSnapshot>, RuntimeError> {
        let records = self.inner.registry.list().await.map_err(storage_error)?;
        let live: HashMap<TerminalRef, Arc<Terminal>> =
            self.inner.terminals.lock().expect("terminals").clone();
        Ok(records
            .into_iter()
            .map(|record| match live.get(&record.terminal) {
                Some(handle) => handle.snapshot(),
                None => record_snapshot(record),
            })
            .collect())
    }

    /// Advance one session's control generation and invalidate every
    /// terminal of that session at its next write commit point.
    pub fn advance_control_generation(
        &self,
        session: &SessionRef,
    ) -> Result<ControlGeneration, RuntimeError> {
        // One critical section covers the session increment and every
        // affected handle bump, so two concurrent advances cannot interleave
        // and regress a terminal's generation below the session's current
        // one. `bump_generation` takes only the terminal's write coordinator
        // mutex and never awaits while this lock is held.
        let mut sessions = self.inner.sessions.lock().expect("sessions");
        let current = *sessions
            .entry(session.clone())
            .or_insert_with(ControlGeneration::first);
        let next = current
            .successor()
            .ok_or(RuntimeError::ControlGenerationExhausted)?;
        sessions.insert(session.clone(), next);
        let terminals: Vec<Arc<Terminal>> = self
            .inner
            .terminals
            .lock()
            .expect("terminals")
            .values()
            .filter(|handle| &handle.reference().session == session)
            .cloned()
            .collect();
        for handle in &terminals {
            handle.bump_generation(next);
        }
        Ok(next)
    }

    /// Track and stop a terminal that completed its spawn transaction but
    /// was refused before live registration. Successful cleanup removes it
    /// from the drain set only while shutdown is not being published;
    /// failures remain visible so a later shutdown reports them.
    fn drain_unregistered(&self, handle: Arc<Terminal>) {
        self.inner
            .draining
            .lock()
            .expect("draining")
            .push(Arc::clone(&handle));
        handle.stop();

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let cleanup = handle.wait_cleanup().await;
            let _gate = inner.start_gate.lock().expect("start gate");
            if !inner.shutdown.load(Ordering::SeqCst)
                && matches!(cleanup, CleanupState::Finished(Ok(())))
            {
                inner
                    .draining
                    .lock()
                    .expect("draining")
                    .retain(|candidate| !Arc::ptr_eq(candidate, &handle));
            }
        });
    }

    /// Stop every terminal and await its detached cleanup, then sweep the
    /// manager-owned cgroup root. Returns an error when any cleanup could
    /// not be verified.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        {
            let _gate = self.inner.start_gate.lock().expect("start gate");
            self.inner.shutdown.store(true, Ordering::SeqCst);
        }
        let _ = self.inner.shutdown_tx.send(true);
        // Wait until every start that passed its first shutdown check has
        // reached its registration decision before collecting terminals. A
        // late start has already been handed to the drain set by the time
        // the count reaches zero, so the sweep below cannot orphan it or
        // remove the manager cgroup root out from under its cleanup.
        let mut in_flight = self.inner.in_flight.subscribe();
        let _ = in_flight.wait_for(|count| *count == 0).await;
        let terminals: Vec<Arc<Terminal>> = self
            .inner
            .terminals
            .lock()
            .expect("terminals")
            .values()
            .cloned()
            .collect();
        // Late starts that were never inserted live: they still need their
        // detached cleanup awaited before the root is removed.
        let draining: Vec<Arc<Terminal>> = self.inner.draining.lock().expect("draining").clone();
        for handle in terminals.iter().chain(draining.iter()) {
            handle.stop();
        }
        let mut failures: Vec<String> = Vec::new();
        for handle in terminals.iter().chain(draining.iter()) {
            match tokio::time::timeout(SHUTDOWN_WAIT, handle.wait_cleanup()).await {
                Ok(CleanupState::Finished(Ok(()))) => {}
                Ok(CleanupState::Finished(Err(detail))) => {
                    failures.push(format!(
                        "{}: {detail}",
                        handle.reference().terminal_id.as_str()
                    ));
                }
                Ok(CleanupState::Pending) | Err(_) => failures.push(format!(
                    "{}: cleanup did not complete",
                    handle.reference().terminal_id.as_str()
                )),
            }
        }
        if let Err(error) = self.inner.cgroup_root.remove() {
            failures.push(format!("sweep manager cgroup root: {error}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::ShutdownIncomplete {
                detail: failures.join("; "),
            })
        }
    }

    fn lookup(&self, terminal: &TerminalRef) -> Option<Arc<Terminal>> {
        self.inner
            .terminals
            .lock()
            .expect("terminals")
            .get(terminal)
            .cloned()
    }

    /// Whether a live terminal's output sink latched degraded (test builds
    /// only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn is_degraded(&self, terminal: &TerminalRef) -> Option<bool> {
        self.lookup(terminal).map(|handle| handle.is_degraded())
    }

    /// Arm the one-shot monitor fault for a live terminal (test builds
    /// only). The next monitor poll is a transient `try_wait` error and
    /// must never fabricate an exit. Returns `false` for an unknown
    /// terminal.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_monitor_fault(&self, terminal: &TerminalRef) -> bool {
        match self.lookup(terminal) {
            Some(handle) => {
                handle.arm_monitor_fault();
                true
            }
            None => false,
        }
    }

    /// Arm the one-shot cleanup-verification fault for a live terminal
    /// (test builds only): the next cleanup runs every real identity step
    /// but withholds the release. Returns `false` for an unknown terminal.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_cleanup_fault(&self, terminal: &TerminalRef) -> bool {
        match self.lookup(terminal) {
            Some(handle) => {
                handle.arm_cleanup_fault();
                true
            }
            None => false,
        }
    }

    /// Arm the pre-registration park for the next start (test builds only):
    /// that start parks at its registration point until
    /// [`Self::release_start_park`] is called, so a test can race shutdown
    /// against it deterministically.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn arm_start_park(&self) {
        self.inner.start_park.arm();
    }

    /// Wait until an armed start has reached its pre-registration park (test
    /// builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn wait_start_parked(&self) {
        self.inner.start_park.wait_reached().await;
    }

    /// Release an armed start's pre-registration park (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn release_start_park(&self) {
        self.inner.start_park.release();
    }

    /// Number of quota slots still occupying capacity (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn occupying_slots(&self) -> usize {
        self.inner.quota.occupying()
    }

    /// Reconcile a cleanup whose release was withheld by the injected
    /// fault (test builds only): persist `released`, free the quota slot
    /// exactly once, and publish success so teardown is clean.
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn reconcile_cleanup_failure(
        &self,
        terminal: &TerminalRef,
    ) -> Result<(), RuntimeError> {
        let handle = self.require(terminal)?;
        handle.reconcile_cleanup_failure().await;
        Ok(())
    }

    /// Await a terminal's detached cleanup to completion (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn await_cleanup(&self, terminal: &TerminalRef) -> Result<(), RuntimeError> {
        let handle = self.require(terminal)?;
        match handle.wait_cleanup().await {
            CleanupState::Finished(Ok(())) => Ok(()),
            CleanupState::Finished(Err(detail)) => Err(RuntimeError::CleanupIncomplete {
                terminal: terminal.clone(),
                detail,
            }),
            CleanupState::Pending => Err(RuntimeError::CleanupIncomplete {
                terminal: terminal.clone(),
                detail: "cleanup pending".into(),
            }),
        }
    }

    fn require(&self, terminal: &TerminalRef) -> Result<Arc<Terminal>, RuntimeError> {
        self.lookup(terminal)
            .ok_or_else(|| RuntimeError::UnknownTerminal(terminal.clone()))
    }

    async fn require_snapshot(
        &self,
        terminal: &TerminalRef,
    ) -> Result<Option<Arc<Terminal>>, RuntimeError> {
        if let Some(handle) = self.lookup(terminal) {
            return Ok(Some(handle));
        }
        if self
            .inner
            .registry
            .load(terminal)
            .await
            .map_err(storage_error)?
            .is_some()
        {
            Ok(None)
        } else {
            Err(RuntimeError::UnknownTerminal(terminal.clone()))
        }
    }
}

fn record_snapshot(record: RuntimeRecord) -> TerminalSnapshot {
    TerminalSnapshot {
        terminal: record.terminal,
        process: record.process,
        output: record.output,
        stopping: record.stopping,
        size: record.size,
        retained_history: None,
    }
}

fn storage_error(error: qingluan_storage::StorageError) -> RuntimeError {
    RuntimeError::Storage {
        detail: error.to_string(),
    }
}

fn cgroup_error(error: crate::cgroup::CgroupError) -> RuntimeError {
    RuntimeError::Cgroup {
        detail: error.to_string(),
    }
}
