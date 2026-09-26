//! S3 terminal-runtime behavior tests through the public `TerminalRuntime`
//! seam. These assert ported Gate B behavior, not probe code: bounded
//! writes, the detached idempotent stop, exact partial counts, forced
//! output close, cgroup-identity cleanup (including `setsid` descendants
//! and SIGTERM-immune processes), generation isolation, exact environment
//! and cwd, and startup Interrupted/reconciliation.
//!
//! The PTY fixture binary (`tests/fixtures/pty_fixture.rs`) is the only
//! program under test control; real commands are never run. Every test
//! uses a temp storage root and a per-test cgroup tag (stable across runs,
//! so a leftover from a crashed run is swept on the next open).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use qingluan_core::terminal::{
    ControlGeneration, EnvironmentSnapshot, ExitResult, ExternalSessionId, GrepLimits, GrepQuery,
    GrepRequest, HistoryPosition, OutputEnd, OutputState, ProcessState, ReadLimits, ReadRequest,
    SessionRef, SessionSource, StartSpec, TerminalId, TerminalRef, TerminalSize, WriteAbort,
};
use qingluan_terminal::{RuntimeError, SendError, SendRejection, TerminalRuntime};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture() -> String {
    env!("CARGO_BIN_EXE_terminal-pty-fixture").to_owned()
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-terminal-s3-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
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

struct Harness {
    root: TempRoot,
    runtime: TerminalRuntime,
    /// Manager-owned cgroup tag, so tests can inspect the per-terminal
    /// cgroup by identity (never by pid).
    tag: String,
}

impl Harness {
    async fn new(name: &str) -> Option<Harness> {
        Self::with_limits(name, 8, 32).await
    }

    async fn with_limits(name: &str, session_limit: usize, global_limit: usize) -> Option<Harness> {
        let root = TempRoot::new(name);
        let tag = format!("qltest-{name}");
        let config = qingluan_terminal::RuntimeConfig::new(tag.clone())
            .with_limits(session_limit, global_limit);
        match TerminalRuntime::open(&root.0, config).await {
            Ok(runtime) => Some(Harness { root, runtime, tag }),
            Err(RuntimeError::Cgroup { detail }) => {
                eprintln!("skipping {name}: cgroup delegation unavailable: {detail}");
                None
            }
            Err(error) => panic!("open {name}: {error}"),
        }
    }

    fn cwd(&self) -> PathBuf {
        self.root.0.clone()
    }

    fn session(&self, id: &str) -> SessionRef {
        SessionRef {
            source: SessionSource::new("test"),
            external_id: ExternalSessionId::new(id),
        }
    }

    fn spec(&self, args: &[&str]) -> StartSpec {
        self.spec_env(args, EnvironmentSnapshot::empty())
    }

    fn spec_env(&self, args: &[&str], env: EnvironmentSnapshot) -> StartSpec {
        StartSpec::new(
            fixture(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            self.cwd().to_string_lossy().into_owned(),
            env,
            TerminalSize {
                rows: 30,
                columns: 120,
            },
        )
    }

    async fn start(&self, session: &SessionRef, spec: StartSpec) -> TerminalRef {
        self.runtime
            .start(session, ControlGeneration::first(), spec)
            .await
            .expect("start")
    }

    async fn finish(&self) {
        self.runtime.shutdown().await.expect("shutdown");
    }
}

fn first() -> ControlGeneration {
    ControlGeneration::first()
}

/// The manager-owned cgroup root for a runtime tag, inside the current
/// delegated subtree. Tests inspect it to prove no cgroup residue.
fn manager_root(tag: &str) -> PathBuf {
    Path::new("/sys/fs/cgroup")
        .join(own_cgroup_relative())
        .join(format!("qingluan-terminal-{tag}"))
}

/// One terminal's cgroup directory (identity-derived, never pid-derived).
fn terminal_cgroup(tag: &str, terminal: &TerminalRef) -> PathBuf {
    manager_root(tag).join(terminal.terminal_id.as_str())
}

/// Directory names of the terminal cgroups currently under a manager root.
fn terminal_cgroup_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return names;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}

/// Live member pids of a cgroup (test-side read; production never uses it).
fn cgroup_members(path: &Path) -> Vec<i32> {
    std::fs::read_to_string(path.join("cgroup.procs"))
        .map(|text| {
            text.lines()
                .filter_map(|line| line.trim().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs only the permission/existence check.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn start_persists_running_and_list_reports_it() {
    let Some(h) = Harness::new("start-running").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;

    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(snapshot.process, ProcessState::Running);
    assert_eq!(snapshot.output, OutputState::Open);
    assert!(!snapshot.stopping);
    assert_eq!(
        snapshot.size,
        TerminalSize {
            rows: 30,
            columns: 120
        }
    );

    let listed = h.runtime.list().await.expect("list");
    assert!(listed.iter().any(|entry| entry.terminal == terminal));

    h.finish().await;
}

#[tokio::test]
async fn send_reaches_program_and_exit_is_committed() {
    let Some(h) = Harness::new("send-reaches").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["read-code"])).await;

    let receipt = h
        .runtime
        .send(&terminal, first(), b"42\n".to_vec())
        .await
        .expect("send");
    assert_eq!(receipt.written_bytes, 3);

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Exited(ExitResult::ExitCode(42)),
        "the program must have received the exact input"
    );
    assert_eq!(snapshot.output, OutputState::Closed(OutputEnd::Eof));

    h.finish().await;
}

#[tokio::test]
async fn environment_snapshot_is_exact_and_not_the_service_environment() {
    let Some(h) = Harness::new("env-exact").await else {
        return;
    };
    let session = h.session("s1");

    // Only the explicit snapshot exists: the service PATH is not inherited.
    let env = EnvironmentSnapshot::new(vec![("QL_MARKER".to_owned(), "present".to_owned())]);
    let terminal = h
        .start(
            &session,
            h.spec_env(&["env-eq", "QL_MARKER", "present"], env),
        )
        .await;
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert_eq!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );

    let terminal = h.start(&session, h.spec(&["env-absent", "PATH"])).await;
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert_eq!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Exited(ExitResult::ExitCode(0)),
        "an empty snapshot must carry no PATH"
    );

    h.finish().await;
}

#[tokio::test]
async fn absolute_cwd_is_applied_and_invalid_cwd_is_rejected() {
    let Some(h) = Harness::new("cwd").await else {
        return;
    };
    let session = h.session("s1");

    let terminal = h
        .start(&session, h.spec(&["cwd-eq", &h.cwd().to_string_lossy()]))
        .await;
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert_eq!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );

    let mut spec = h.spec(&["sleep"]);
    spec.cwd = "/definitely/not/a/directory".to_owned();
    let error = h.runtime.start(&session, first(), spec).await.unwrap_err();
    assert!(matches!(error, RuntimeError::StartRejected { .. }));

    h.finish().await;
}

#[tokio::test]
async fn oversize_send_is_rejected() {
    let Some(h) = Harness::new("oversize").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;

    let too_big = vec![0u8; 256 * 1024 + 1];
    assert_eq!(
        h.runtime.send(&terminal, first(), too_big).await,
        Err(SendError::Rejected(SendRejection::Oversize))
    );

    // Exactly the bound is accepted (it may block or complete, but it is
    // never rejected as oversize).
    let at_bound = vec![0u8; 256 * 1024];
    let send = h.runtime.send(&terminal, first(), at_bound);
    tokio::pin!(send);
    tokio::select! {
        result = &mut send => {
            assert!(!matches!(result, Err(SendError::Rejected(SendRejection::Oversize))));
        }
        _ = tokio::time::sleep(Duration::from_millis(200)) => {
            // Blocked at the accepted bound is the expected outcome.
        }
    }

    h.finish().await;
}

#[tokio::test]
async fn stale_control_generation_is_rejected() {
    let Some(h) = Harness::new("generation").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["echo"])).await;

    let next = h
        .runtime
        .advance_control_generation(&session)
        .expect("advance");
    assert_eq!(next.get(), 2);
    assert_eq!(
        h.runtime.send(&terminal, first(), b"stale".to_vec()).await,
        Err(SendError::Rejected(SendRejection::ControlLost))
    );
    assert!(
        h.runtime
            .send(&terminal, next, b"ok\n".to_vec())
            .await
            .is_ok()
    );

    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert_eq!(
        h.runtime.send(&terminal, next, b"after".to_vec()).await,
        Err(SendError::Rejected(SendRejection::Stopped))
    );

    h.finish().await;
}

#[tokio::test]
async fn stop_returns_immediately_and_is_idempotent() {
    let Some(h) = Harness::new("stop-idempotent").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;

    let started = std::time::Instant::now();
    let snapshot = h.runtime.stop(&terminal).await.expect("stop");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "stop must return immediately after the atomic intent"
    );
    assert!(snapshot.stopping, "the stop intent is visible immediately");

    // A repeated stop shares the same cleanup and also returns immediately.
    let repeated = h.runtime.stop(&terminal).await.expect("repeat stop");
    assert!(repeated.stopping);

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let final_snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert!(!final_snapshot.stopping);
    assert_eq!(final_snapshot.output, OutputState::Closed(OutputEnd::Eof));

    h.finish().await;
}

#[tokio::test]
async fn send_after_stop_is_rejected() {
    let Some(h) = Harness::new("send-after-stop").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;

    h.runtime.stop(&terminal).await.expect("stop");
    assert_eq!(
        h.runtime.send(&terminal, first(), b"late".to_vec()).await,
        Err(SendError::Rejected(SendRejection::Stopped))
    );

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

#[tokio::test]
async fn blocked_write_stop_reports_exact_partial_count() {
    let Some(h) = Harness::new("blocked-write").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["raw-sleep"])).await;

    let payload = vec![b'x'; 256 * 1024];
    let runtime = h.runtime.clone();
    let target = terminal.clone();
    let send = tokio::spawn(async move { runtime.send(&target, first(), payload).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    h.runtime.stop(&terminal).await.expect("stop");
    let result = send.await.expect("send task");
    match result {
        Err(SendError::Partial(partial)) => {
            assert!(partial.written_bytes > 0, "some bytes were written");
            assert!(
                partial.written_bytes < 256 * 1024,
                "the write did not complete"
            );
            assert_eq!(partial.reason, WriteAbort::StopIntent);
        }
        other => panic!("expected an exact partial write, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the blocked write aborts promptly at the stop"
    );

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

#[tokio::test]
async fn queue_full_is_rejected() {
    let Some(h) = Harness::new("queue-full").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;

    let mut tasks = Vec::new();
    for _ in 0..6 {
        let runtime = h.runtime.clone();
        let target = terminal.clone();
        tasks.push(tokio::spawn(async move {
            runtime.send(&target, first(), vec![b'q'; 64 * 1024]).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = h.runtime.stop(&terminal).await;
    let mut queue_full = 0;
    for task in tasks {
        if let Err(SendError::Rejected(SendRejection::QueueFull)) = task.await.expect("task") {
            queue_full += 1;
        }
    }
    assert!(
        queue_full >= 3,
        "the bounded queue (2 + 1 in flight) must reject the excess, got {queue_full}"
    );

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

#[tokio::test]
async fn resize_is_applied() {
    let Some(h) = Harness::new("resize").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["resize-code"])).await;

    h.runtime
        .resize(
            &terminal,
            first(),
            TerminalSize {
                rows: 45,
                columns: 100,
            },
        )
        .await
        .expect("resize");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.size,
        TerminalSize {
            rows: 45,
            columns: 100
        }
    );

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert_eq!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Exited(ExitResult::ExitCode(45)),
        "the child observed the new window size"
    );

    h.finish().await;
}

#[tokio::test]
async fn child_holding_pty_is_reclaimed_and_output_closes() {
    let Some(h) = Harness::new("child-hold").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["child-hold"])).await;

    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    assert!(
        matches!(snapshot.output, OutputState::Closed(_)),
        "output must close once the holding child is reclaimed"
    );

    h.finish().await;
}

#[tokio::test]
async fn escaped_child_forces_the_bounded_output_close() {
    let Some(h) = Harness::new("escaped-child").await else {
        return;
    };
    let session = h.session("s1");
    // The child moves itself into the test process's own scope (out of the
    // terminal cgroup) and holds the pty, so nothing the stop can kill is
    // holding the master open.
    let scope = format!("/sys/fs/cgroup/{}", own_cgroup_relative().display());
    let terminal = h.start(&session, h.spec(&["child-escape", &scope])).await;

    // Give the child time to move out of the terminal cgroup before the
    // stop scans it.
    tokio::time::sleep(Duration::from_millis(400)).await;
    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.output,
        OutputState::Closed(OutputEnd::ForcedClose),
        "the bounded output-close timeout is the unique ForcedClose winner"
    );

    // The escaped child self-terminates shortly (it is outside the
    // terminal cgroup, an intentional non-guarantee); wait for it so the
    // test leaves no residue.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    h.finish().await;
}

#[tokio::test]
async fn setsid_child_is_reclaimed_by_cgroup() {
    let Some(h) = Harness::new("setsid").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["setsid-child"])).await;

    h.runtime.stop(&terminal).await.expect("stop");
    // A cleanup failure would leave the cgroup populated (the escaped-session
    // SIGTERM-immune child); an Ok cleanup proves cgroup.kill reclaimed it.
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert!(matches!(
        h.runtime.snapshot(&terminal).await.unwrap().output,
        OutputState::Closed(_)
    ));

    h.finish().await;
}

#[tokio::test]
async fn term_immune_child_is_killed_after_the_grace() {
    let Some(h) = Harness::new("term-immune").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["term-immune"])).await;

    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Exited(ExitResult::Signal(libc::SIGKILL)),
        "the SIGTERM-immune process must be reclaimed by cgroup.kill"
    );

    h.finish().await;
}

#[tokio::test]
async fn natural_exit_commits_process_and_output_independently() {
    let Some(h) = Harness::new("natural-exit").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["tail"])).await;

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    assert_eq!(
        snapshot.output,
        OutputState::Closed(OutputEnd::Eof),
        "the trailing no-newline output still ends as a normal Eof"
    );

    h.finish().await;
}

#[tokio::test]
async fn shutdown_stops_all_terminals() {
    let Some(h) = Harness::new("shutdown").await else {
        return;
    };
    let session = h.session("s1");
    let first_terminal = h.start(&session, h.spec(&["sleep"])).await;
    let second_terminal = h.start(&session, h.spec(&["sleep"])).await;

    h.runtime.shutdown().await.expect("shutdown");
    for terminal in [&first_terminal, &second_terminal] {
        let snapshot = h.runtime.snapshot(terminal).await.expect("snapshot");
        assert!(!snapshot.stopping);
        assert!(matches!(snapshot.output, OutputState::Closed(_)));
        assert!(matches!(snapshot.process, ProcessState::Exited(_)));
    }
    // A second shutdown is a successful no-op.
    h.runtime.shutdown().await.expect("idempotent shutdown");
}

#[tokio::test]
async fn failed_start_releases_quota() {
    let Some(h) = Harness::with_limits("failed-start", 1, 1).await else {
        return;
    };
    let session = h.session("s1");

    let mut spec = h.spec(&["sleep"]);
    spec.program = "/definitely/not/a/program".to_owned();
    let error = h.runtime.start(&session, first(), spec).await.unwrap_err();
    assert!(matches!(error, RuntimeError::StartRejected { .. }));

    // The failed reservation must not block the only slot.
    let terminal = h.start(&session, h.spec(&["tail"])).await;
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

#[tokio::test]
async fn quota_limits_are_enforced() {
    let Some(h) = Harness::with_limits("quota", 1, 2).await else {
        return;
    };
    let s1 = h.session("s1");
    let s2 = h.session("s2");

    let first_terminal = h.start(&s1, h.spec(&["sleep"])).await;
    // The same session is at its session limit.
    let error = h
        .runtime
        .start(&s1, first(), h.spec(&["sleep"]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, RuntimeError::StartRejected { .. }),
        "the session limit must refuse a second terminal in the same session"
    );
    let second_terminal = h.start(&s2, h.spec(&["sleep"])).await;
    // The global limit (2) is now reached.
    let third = h.session("s3");
    let error = h
        .runtime
        .start(&third, first(), h.spec(&["sleep"]))
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::StartRejected { .. }));

    let _ = h.runtime.stop(&first_terminal).await;
    let _ = h.runtime.stop(&second_terminal).await;
    h.runtime
        .await_cleanup(&first_terminal)
        .await
        .expect("cleanup");
    h.runtime
        .await_cleanup(&second_terminal)
        .await
        .expect("cleanup");
    h.finish().await;
}

#[tokio::test]
async fn open_marks_unfinished_records_interrupted_and_sweeps_leftover_cgroups() {
    let root = TempRoot::new("recovery");
    let tag = "qltest-recovery".to_owned();

    // Create a durable record that is left `running`, as a crash would.
    let session = SessionRef {
        source: SessionSource::new("test"),
        external_id: ExternalSessionId::new("s1"),
    };
    let terminal = TerminalRef {
        session,
        // A fixed canonical UUID; the runtime and storage require canonical
        // text, but recovery never reattaches or signals anything by pid.
        terminal_id: qingluan_core::terminal::TerminalId::new(
            "01890000-0000-7000-8000-000000000001",
        ),
    };
    let registry = qingluan_storage::RuntimeRegistry::open(&root.0)
        .await
        .expect("registry");
    registry
        .begin(
            &terminal,
            TerminalSize {
                rows: 30,
                columns: 120,
            },
        )
        .await
        .expect("begin");
    registry
        .mark_running(&terminal)
        .await
        .expect("mark running");

    // A leftover manager-owned terminal cgroup from the crashed run.
    let manager_root = Path::new("/sys/fs/cgroup")
        .join(own_cgroup_relative())
        .join(format!("qingluan-terminal-{tag}"));
    let leftover = manager_root.join("leftover-t1");
    std::fs::create_dir_all(&leftover).expect("create leftover cgroup");
    assert!(leftover.is_dir());

    let config = qingluan_terminal::RuntimeConfig::new(tag);
    let runtime = TerminalRuntime::open(&root.0, config)
        .await
        .expect("reopen runtime");

    let snapshot = runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Interrupted,
        "startup marks the record Interrupted only, never a fabricated exit"
    );
    assert_eq!(snapshot.output, OutputState::Closed(OutputEnd::Interrupted));
    assert!(
        !leftover.exists(),
        "the manager-owned leftover cgroup must be reconciled by identity"
    );

    runtime.shutdown().await.expect("shutdown");
}

fn own_cgroup_relative() -> PathBuf {
    let text = std::fs::read_to_string("/proc/self/cgroup").expect("cgroup");
    let line = text
        .lines()
        .find(|line| line.starts_with("0::"))
        .expect("unified cgroup line");
    let path = line.trim_start_matches("0::").trim_start_matches('/');
    Path::new(path).to_path_buf()
}

// ── Ported Gate B scenarios (production runtime seam) ─────────────────────
//
// The probe observed identity, input delivery, and process state through
// its own event log and output tail. The production S3 seam exposes no pid
// and no output read (that is S4), so the equivalent observations go
// through a test-only fixture that writes facts to a file, through the
// manager-owned cgroup by terminal identity, and through the typed
// snapshot. Nothing here uses a raw pid or a `/proc` fallback for cleanup;
// the test-side cgroup/proc reads only observe.

/// scenario identity: the root is a session and process-group leader with
/// itself as the pts foreground group, and lives in its own terminal
/// cgroup.
#[tokio::test]
async fn identity_is_a_session_leader_in_its_terminal_cgroup() {
    let Some(h) = Harness::new("identity").await else {
        return;
    };
    let session = h.session("s1");
    let file = h.cwd().join("identity.txt");
    let terminal = h
        .start(&session, h.spec(&["identity", &file.to_string_lossy()]))
        .await;
    assert!(
        wait_for_file(&file, Duration::from_secs(5)),
        "the fixture never wrote its identity"
    );
    let text = std::fs::read_to_string(&file).expect("identity file");
    let field = |name: &str| -> i64 {
        text.split_whitespace()
            .find_map(|token| token.strip_prefix(&format!("{name}=")))
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("missing {name} in {text:?}"))
    };
    let pid = field("pid");
    assert_eq!(pid, field("sid"), "the root must lead its own session");
    assert_eq!(pid, field("pgrp"), "the root must lead its own group");
    assert_eq!(pid, field("fg"), "the pts foreground group is the root");
    let cgroup = text
        .split("cgroup=")
        .nth(1)
        .map(str::trim)
        .unwrap_or_default();
    assert!(
        cgroup.contains(&format!("qingluan-terminal-{}", h.tag)),
        "root must join its terminal cgroup, got {cgroup:?}"
    );
    assert!(
        cgroup.ends_with(terminal.terminal_id.as_str()),
        "root cgroup must be the terminal's own identity: {cgroup:?}"
    );
    assert!(
        cgroup_members(&terminal_cgroup(&h.tag, &terminal)).contains(&(pid as i32)),
        "the terminal cgroup must list the root"
    );

    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

/// scenario write-bytes: a mixed UTF-8/control payload (in a `Vec` with
/// megabytes of spare capacity) is delivered byte-for-byte, and the exact
/// written count is reported.
#[tokio::test]
async fn byte_writes_deliver_the_exact_mixed_payload() {
    let Some(h) = Harness::new("byte-writes").await else {
        return;
    };
    let session = h.session("s1");
    let file = h.cwd().join("bytecount.txt");
    let terminal = h
        .start(
            &session,
            h.spec(&["bytecount", "300", &file.to_string_lossy()]),
        )
        .await;
    assert!(
        wait_for_file(&h.cwd().join("bytecount.txt.ready"), Duration::from_secs(5)),
        "the fixture never entered raw mode"
    );

    let mut payload: Vec<u8> = Vec::with_capacity(8 * 1024 * 1024);
    payload.extend_from_slice(b"line1\n");
    payload.extend_from_slice("中文多行\nsecond-line\n".as_bytes());
    payload.extend_from_slice(&[0x03, 0x1b, b'[', b'2', b'K']);
    payload.extend_from_slice(b"tail-no-newline");
    while payload.len() < 300 {
        payload.push(b'A');
    }
    payload.truncate(300);
    assert!(payload.capacity() >= 8 * 1024 * 1024);
    let expected_sum: u64 = payload
        .iter()
        .fold(0u64, |acc, byte| acc.wrapping_add(u64::from(*byte)));

    let receipt = h
        .runtime
        .send(&terminal, first(), payload)
        .await
        .expect("send");
    assert_eq!(receipt.written_bytes, 300);
    assert!(wait_for_file(&file, Duration::from_secs(5)));
    assert_eq!(
        std::fs::read_to_string(&file).expect("bytecount file"),
        format!("count=300 sum={expected_sum}"),
        "the fixture must observe exactly the offered bytes"
    );

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

/// scenario shell-jobs: a job-control shell's foreground and background
/// jobs all stay in the terminal cgroup and are reclaimed by the stop.
#[tokio::test]
async fn shell_job_process_groups_are_all_reclaimed_on_stop() {
    if !Path::new("/bin/sh").exists() {
        eprintln!("skipping shell-jobs: /bin/sh unavailable");
        return;
    }
    let Some(h) = Harness::new("shell-jobs").await else {
        return;
    };
    let session = h.session("s1");
    let mut spec = h.spec(&["-m", "-c", "sleep 30 & sleep 30"]);
    spec.program = "/bin/sh".to_owned();
    // The shell needs PATH to find `sleep`; this is still an explicit
    // snapshot, never the service environment by inheritance.
    spec.env = EnvironmentSnapshot::new(vec![(
        "PATH".to_owned(),
        std::env::var("PATH").unwrap_or_default(),
    )]);
    let terminal = h.start(&session, spec).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let cgroup = terminal_cgroup(&h.tag, &terminal);
    let members = cgroup_members(&cgroup);
    assert!(
        members.len() >= 2,
        "expected the shell and its jobs in the terminal cgroup, got {members:?}"
    );

    h.runtime.stop(&terminal).await.expect("stop");
    // A cleanup failure would leave the cgroup populated; Ok proves every
    // job member, in every job process group, was reclaimed.
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert!(
        !cgroup.exists(),
        "the terminal cgroup must be removed after cleanup"
    );
    h.finish().await;
}

/// scenario generation-race: 100 concurrent control-generation switches
/// racing in-flight writes never produce an illegal outcome, and a write at
/// the final generation still commits.
#[tokio::test]
async fn generation_race_keeps_every_send_legal_and_final_generation_commits() {
    let Some(h) = Harness::new("generation-race").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["raw-sleep"])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let latest = Arc::new(AtomicU64::new(first().get()));
    let sender_done = Arc::new(AtomicBool::new(false));

    let bumper = {
        let runtime = h.runtime.clone();
        let session = session.clone();
        let latest = Arc::clone(&latest);
        let sender_done = Arc::clone(&sender_done);
        tokio::spawn(async move {
            // Bump until the sender is done, so an in-flight blocked write
            // is always aborted within one bump interval and the loop can
            // never deadlock on a stable generation.
            while !sender_done.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
                let next = runtime
                    .advance_control_generation(&session)
                    .expect("advance");
                latest.store(next.get(), Ordering::SeqCst);
            }
            latest.load(Ordering::SeqCst)
        })
    };

    let sender = {
        let runtime = h.runtime.clone();
        let target = terminal.clone();
        let latest = Arc::clone(&latest);
        let sender_done = Arc::clone(&sender_done);
        tokio::spawn(async move {
            let (mut complete, mut aborted, mut rejected) = (0usize, 0usize, 0usize);
            for _ in 0..300 {
                let generation =
                    ControlGeneration::new(latest.load(Ordering::SeqCst)).expect("live generation");
                match runtime
                    .send(&target, generation, vec![b'G'; 64 * 1024])
                    .await
                {
                    Ok(receipt) => {
                        assert_eq!(receipt.written_bytes, 64 * 1024);
                        complete += 1;
                    }
                    Err(SendError::Partial(partial)) => {
                        assert_eq!(partial.reason, WriteAbort::ControlLost);
                        assert!(partial.written_bytes < 64 * 1024);
                        aborted += 1;
                    }
                    Err(SendError::Rejected(SendRejection::ControlLost)) => rejected += 1,
                    other => panic!("illegal race outcome: {other:?}"),
                }
            }
            sender_done.store(true, Ordering::SeqCst);
            (complete, aborted, rejected)
        })
    };

    let (complete, aborted, rejected) = sender.await.expect("sender task");
    let final_generation = bumper.await.expect("bumper task");
    assert!(final_generation >= 2, "the race must switch generations");
    assert!(
        aborted + rejected > 0,
        "a 100-switch race must actually lose control on some send (complete={complete})"
    );

    // The final generation is current, not stale: a send claimed on it must
    // not be refused with ControlLost. The tty input buffer is saturated by
    // the race, so the write may block; a short timeout then proves it was
    // accepted at its commit point rather than rejected.
    let final_gen = ControlGeneration::new(final_generation).expect("final generation");
    let attempt = tokio::time::timeout(
        Duration::from_millis(300),
        h.runtime.send(&terminal, final_gen, b"ok\n".to_vec()),
    )
    .await;
    match attempt {
        Ok(Ok(receipt)) => assert_eq!(receipt.written_bytes, 3),
        Ok(Err(SendError::Rejected(SendRejection::ControlLost))) => {
            panic!("the final generation was refused as stale")
        }
        Ok(Err(SendError::Partial(partial))) if partial.reason == WriteAbort::ControlLost => {
            panic!("the final generation aborted as stale")
        }
        Ok(Err(other)) => panic!("final-generation write failed: {other:?}"),
        // Still in flight: accepted at the commit point, blocked by the
        // saturated buffer. The stop below aborts it.
        Err(_) => {}
    }

    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

/// scenario write-deadline: the production 10 s deadline is a bound, not a
/// short watchdog; a blocked write stays in flight past a second and is
/// aborted at the stop with its exact partial count. (`limits` pins the
/// 10 s value.)
#[tokio::test]
async fn blocked_write_is_bounded_by_the_deadline_not_a_watchdog() {
    let Some(h) = Harness::new("write-deadline").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["raw-sleep"])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let runtime = h.runtime.clone();
    let target = terminal.clone();
    let send =
        tokio::spawn(async move { runtime.send(&target, first(), vec![b'D'; 256 * 1024]).await });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !send.is_finished(),
        "the write must still be blocked well before the 10 s deadline"
    );

    let started = Instant::now();
    h.runtime.stop(&terminal).await.expect("stop");
    match send.await.expect("send task") {
        Err(SendError::Partial(partial)) => {
            assert!(partial.written_bytes > 0 && partial.written_bytes < 256 * 1024);
            assert_eq!(partial.reason, WriteAbort::StopIntent);
        }
        other => panic!("expected a partial write at the stop, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2));

    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    h.finish().await;
}

/// scenario service-shutdown: a service shutdown aborts a blocked write in
/// bounded time instead of waiting out the deadline, and the lifecycle
/// still completes exactly once.
#[tokio::test]
async fn service_shutdown_aborts_a_blocked_write_promptly() {
    let Some(h) = Harness::new("svc-shutdown").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["raw-sleep"])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let runtime = h.runtime.clone();
    let target = terminal.clone();
    let send =
        tokio::spawn(async move { runtime.send(&target, first(), vec![b'S'; 256 * 1024]).await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    h.runtime.shutdown().await.expect("shutdown");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "shutdown must not wait out the write deadline"
    );
    // Shutdown signals the shutdown latch before it commits the stop, but
    // both are legal abort reasons for the same bounded partial write.
    match send.await.expect("send task") {
        Err(SendError::Partial(partial)) => {
            assert!(partial.written_bytes < 256 * 1024);
            assert!(
                matches!(
                    partial.reason,
                    WriteAbort::ServiceShutdown | WriteAbort::StopIntent
                ),
                "expected a shutdown/stop abort, got {:?}",
                partial.reason
            );
        }
        other => panic!("expected a bounded partial write, got {other:?}"),
    }
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert!(matches!(snapshot.output, OutputState::Closed(_)));
    assert!(matches!(snapshot.process, ProcessState::Exited(_)));
    h.finish().await;
}

/// scenario close-race (a): a natural EOF wins; a later stop returns the
/// same committed state and re-runs no cleanup.
#[tokio::test]
async fn natural_eof_close_then_stop_is_existing_state() {
    let Some(h) = Harness::new("close-race").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["tail"])).await;
    h.runtime
        .await_cleanup(&terminal)
        .await
        .expect("natural finalize");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    assert_eq!(snapshot.output, OutputState::Closed(OutputEnd::Eof));

    let stopped = h.runtime.stop(&terminal).await.expect("stop");
    assert!(!stopped.stopping);
    assert_eq!(stopped.output, OutputState::Closed(OutputEnd::Eof));
    let again = h.runtime.stop(&terminal).await.expect("stop again");
    assert_eq!(again.output, stopped.output);
    assert_eq!(again.process, stopped.process);
    h.finish().await;
}

/// scenario term-fork: a root that traps SIGTERM and forks SIGTERM-immune
/// children cannot be stopped by TERM; `cgroup.kill` is the fork-safe fixed
/// point.
#[tokio::test]
async fn term_forking_root_is_reclaimed_by_cgroup_kill() {
    let Some(h) = Harness::new("term-fork").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["term-fork"])).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let started = Instant::now();
    h.runtime.stop(&terminal).await.expect("stop");
    // Ok proves the cgroup emptied: every late TERM-time fork was reclaimed.
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the stop must be bounded"
    );
    assert!(!terminal_cgroup(&h.tag, &terminal).exists());
    assert!(matches!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Exited(_)
    ));
    h.finish().await;
}

/// scenario start-faults + rollback: every staged start refusal (validation,
/// spawn failure, quota exhaustion) rolls back its reservation, cgroup, and
/// record, leaving the slot reusable and no cgroup residue.
#[tokio::test]
async fn staged_start_rollback_leaves_no_quota_or_cgroup_residue() {
    let Some(h) = Harness::with_limits("start-rollback", 1, 2).await else {
        return;
    };
    let session = h.session("s1");
    let root = manager_root(&h.tag);
    let cases: Vec<(&str, StartSpec)> = vec![
        ("empty program", {
            let mut spec = h.spec(&["sleep"]);
            spec.program = String::new();
            spec
        }),
        ("relative cwd", {
            let mut spec = h.spec(&["sleep"]);
            spec.cwd = "relative/path".to_owned();
            spec
        }),
        ("missing cwd", {
            let mut spec = h.spec(&["sleep"]);
            spec.cwd = "/definitely/not/a/directory".to_owned();
            spec
        }),
        ("zero size", {
            let mut spec = h.spec(&["sleep"]);
            spec.size = TerminalSize {
                rows: 0,
                columns: 0,
            };
            spec
        }),
        ("missing program", {
            let mut spec = h.spec(&["sleep"]);
            spec.program = "/definitely/not/a/program".to_owned();
            spec
        }),
    ];
    for (label, spec) in cases {
        let error = h.runtime.start(&session, first(), spec).await.unwrap_err();
        assert!(
            matches!(error, RuntimeError::StartRejected { .. }),
            "{label}: expected StartRejected, got {error:?}"
        );
        assert_eq!(
            terminal_cgroup_names(&root).len(),
            0,
            "{label} must not leave a cgroup"
        );
        // The single session slot is free again after the rolled-back start.
        let terminal = h.start(&session, h.spec(&["sleep"])).await;
        h.runtime.stop(&terminal).await.expect("stop");
        h.runtime.await_cleanup(&terminal).await.expect("cleanup");
        assert_eq!(terminal_cgroup_names(&root).len(), 0, "{label}: cleanup");
    }

    // A quota-exhausted start is also a staged refusal with no residue.
    let held = h.start(&session, h.spec(&["sleep"])).await;
    let error = h
        .runtime
        .start(&session, first(), h.spec(&["sleep"]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, RuntimeError::StartRejected { .. }),
        "quota refusal must be StartRejected, got {error:?}"
    );
    assert_eq!(terminal_cgroup_names(&root).len(), 1);
    h.runtime.stop(&held).await.expect("stop held");
    h.runtime.await_cleanup(&held).await.expect("cleanup held");
    h.finish().await;
}

/// scenario quota-competition: concurrent, repeated stops of competing
/// terminals share one cleanup and release each slot exactly once, so the
/// freed capacity is reusable and nothing stays occupied.
#[tokio::test]
async fn quota_competition_releases_each_slot_exactly_once() {
    let Some(h) = Harness::with_limits("quota-competition", 1, 2).await else {
        return;
    };
    let s1 = h.session("s1");
    let s2 = h.session("s2");
    let s3 = h.session("s3");
    let t1 = h.start(&s1, h.spec(&["sleep"])).await;
    let t2 = h.start(&s2, h.spec(&["sleep"])).await;
    // Both limits are reached: a new session and a second terminal in s1 are
    // refused without side effects.
    assert!(
        h.runtime
            .start(&s3, first(), h.spec(&["sleep"]))
            .await
            .is_err()
    );
    assert!(
        h.runtime
            .start(&s1, first(), h.spec(&["sleep"]))
            .await
            .is_err()
    );

    let (a, b) = tokio::join!(h.runtime.stop(&t1), h.runtime.stop(&t1));
    a.expect("stop 1");
    b.expect("stop 1 repeat");
    let (c, d) = tokio::join!(h.runtime.stop(&t2), h.runtime.stop(&t2));
    c.expect("stop 2");
    d.expect("stop 2 repeat");
    h.runtime.await_cleanup(&t1).await.expect("cleanup 1");
    h.runtime
        .await_cleanup(&t1)
        .await
        .expect("cleanup 1 repeat");
    h.runtime.await_cleanup(&t2).await.expect("cleanup 2");

    // Exactly two slots freed: both are reusable, and a third is not.
    let t4 = h.start(&s1, h.spec(&["sleep"])).await;
    let t5 = h.start(&s3, h.spec(&["sleep"])).await;
    assert!(
        h.runtime
            .start(&s2, first(), h.spec(&["sleep"]))
            .await
            .is_err()
    );
    h.runtime.stop(&t4).await.expect("stop 4");
    h.runtime.stop(&t5).await.expect("stop 5");
    h.runtime.await_cleanup(&t4).await.expect("cleanup 4");
    h.runtime.await_cleanup(&t5).await.expect("cleanup 5");
    h.finish().await;
}

/// scenario registry-interrupted: startup recovery marks records
/// Interrupted with no fabricated exit, and never signals a live process it
/// only knows by identity.
#[tokio::test]
async fn startup_recovery_marks_interrupted_and_never_signals_a_live_process() {
    let root = TempRoot::new("recovery-signal");
    let tag = "qltest-recovery-signal".to_owned();

    // A live process outside every manager-owned cgroup. Recovery must leave
    // it untouched (its identity cannot be trusted across a crash).
    let mut child = std::process::Command::new(fixture())
        .arg("sleep")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn standalone fixture");
    let pid = child.id() as i32;

    let terminal = TerminalRef {
        session: SessionRef {
            source: SessionSource::new("test"),
            external_id: ExternalSessionId::new("s1"),
        },
        terminal_id: TerminalId::new("01890000-0000-7000-8000-0000000000aa"),
    };
    {
        let registry = qingluan_storage::RuntimeRegistry::open(&root.0)
            .await
            .expect("registry");
        registry
            .begin(
                &terminal,
                TerminalSize {
                    rows: 30,
                    columns: 120,
                },
            )
            .await
            .expect("begin");
        registry.mark_running(&terminal).await.expect("running");
    }

    let runtime = TerminalRuntime::open(&root.0, qingluan_terminal::RuntimeConfig::new(tag))
        .await
        .expect("reopen runtime");
    let snapshot = runtime.snapshot(&terminal).await.expect("snapshot");
    assert_eq!(
        snapshot.process,
        ProcessState::Interrupted,
        "recovery marks Interrupted only"
    );
    assert_eq!(snapshot.output, OutputState::Closed(OutputEnd::Interrupted));

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        process_alive(pid),
        "recovery must not signal a process known only by identity"
    );
    child.kill().expect("kill standalone fixture");
    child.wait().expect("reap standalone fixture");
    runtime.shutdown().await.expect("shutdown");
}

/// scenario stop-cancellation: a stop waiter that is cancelled must not
/// cancel the detached cleanup; a later waiter observes the same completion.
#[tokio::test]
async fn cancelling_a_stop_waiter_does_not_cancel_the_detached_cleanup() {
    let Some(h) = Harness::new("stop-cancellation").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;
    h.runtime.stop(&terminal).await.expect("stop");

    // The committed stop runs detached; cancelling a wait for it must not
    // abort that cleanup.
    let runtime = h.runtime.clone();
    let target = terminal.clone();
    let waiter = tokio::spawn(async move { runtime.await_cleanup(&target).await });
    waiter.abort();
    assert!(waiter.await.expect_err("cancelled").is_cancelled());

    h.runtime
        .await_cleanup(&terminal)
        .await
        .expect("cleanup after a cancelled waiter");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert!(matches!(snapshot.output, OutputState::Closed(_)));
    assert!(!snapshot.stopping);
    h.finish().await;
}

/// No-residue check: after a clean shutdown the manager-owned cgroup root is
/// gone and the temp storage root can be removed.
#[tokio::test]
async fn clean_shutdown_leaves_no_cgroup_or_temp_residue() {
    let Some(h) = Harness::new("residue").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;
    h.runtime.stop(&terminal).await.expect("stop");
    let root = manager_root(&h.tag);
    h.runtime.shutdown().await.expect("shutdown");
    assert!(
        !root.exists(),
        "the manager-owned cgroup root must be removed"
    );
    assert_eq!(terminal_cgroup_names(&root).len(), 0);

    let temp = h.root.0.clone();
    drop(h);
    assert!(!temp.exists(), "the temp storage root must be gone");
}

/// scenario monitor-fault: a transient monitor poll failure commits no
/// exit and sets no exited flag; a real stop still reaps exactly once.
#[tokio::test]
async fn monitor_fault_never_fabricates_an_exit() {
    let Some(h) = Harness::new("monitor-fault").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;
    assert!(h.runtime.set_monitor_fault(&terminal), "fault armed");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        h.runtime.snapshot(&terminal).await.unwrap().process,
        ProcessState::Running,
        "a monitor fault must not fabricate ProcessExited"
    );

    // A later real exit is still reaped exactly once.
    h.runtime.stop(&terminal).await.expect("stop");
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert!(matches!(snapshot.process, ProcessState::Exited(_)));
    assert!(matches!(snapshot.output, OutputState::Closed(_)));
    h.finish().await;
}

/// scenario rollback/cleanup-failure: an unverifiable cleanup returns
/// `CleanupIncomplete`, keeps the quota slot occupied (`stopping` stays
/// true, no release), and — because every real identity cleanup step still
/// ran — leaves no cgroup or process residue. A test-only reconciliation
/// then frees the occupied record so teardown is clean.
#[tokio::test]
async fn cleanup_verification_failure_keeps_quota_occupied_without_residue() {
    let Some(h) = Harness::with_limits("cleanup-failure", 1, 1).await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h.start(&session, h.spec(&["sleep"])).await;
    assert!(h.runtime.set_cleanup_fault(&terminal), "fault armed");

    h.runtime.stop(&terminal).await.expect("stop");
    let error = h
        .runtime
        .await_cleanup(&terminal)
        .await
        .expect_err("cleanup must not verify");
    assert!(
        matches!(error, RuntimeError::CleanupIncomplete { .. }),
        "expected CleanupIncomplete, got {error:?}"
    );

    // The release is withheld: the only slot stays occupied, so the same
    // session cannot start another terminal.
    let refused = h
        .runtime
        .start(&session, first(), h.spec(&["sleep"]))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, RuntimeError::StartRejected { .. }),
        "the occupied slot must refuse a new start, got {refused:?}"
    );
    let snapshot = h.runtime.snapshot(&terminal).await.expect("snapshot");
    assert!(
        snapshot.stopping,
        "a withheld cleanup stays visibly stopping"
    );
    assert!(matches!(snapshot.process, ProcessState::Exited(_)));
    assert!(matches!(snapshot.output, OutputState::Closed(_)));
    // The identity cleanup already ran even though the release was withheld.
    assert!(
        !terminal_cgroup(&h.tag, &terminal).exists(),
        "no cgroup residue despite the withheld release"
    );

    // Test-only reconciliation frees the occupied record; the slot is then
    // reusable and shutdown is clean.
    h.runtime
        .reconcile_cleanup_failure(&terminal)
        .await
        .expect("reconcile");
    let reused = h.start(&session, h.spec(&["sleep"])).await;
    h.runtime.stop(&reused).await.expect("stop reused");
    h.runtime
        .await_cleanup(&reused)
        .await
        .expect("cleanup reused");
    h.finish().await;
}

/// Race-1 (advance vs advance): two concurrent control-generation advances
/// are serialized as one critical section, so they always land on two
/// consecutive generations, the final session generation is the newest, and
/// every terminal's write coordinator rejects both older tokens with no byte
/// committed.
#[tokio::test]
async fn concurrent_advances_never_regress_a_terminal_generation() {
    let Some(h) = Harness::new("advance-race").await else {
        return;
    };
    let session = h.session("s1");
    let first_terminal = h.start(&session, h.spec(&["raw-sleep"])).await;
    let second_terminal = h.start(&session, h.spec(&["raw-sleep"])).await;

    // Each round lines both advances up on a barrier and repeats, so the
    // harmful interleaving (one advance reading the session generation
    // before the other's increment lands) is forced rather than left to
    // chance. The two callers must return consecutive generations; the old
    // two-phase advance could leave one terminal a generation behind.
    const ROUNDS: u64 = 200;
    let runtime = h.runtime.clone();
    let racing_session = session.clone();
    tokio::task::spawn_blocking(move || {
        for round in 0..ROUNDS {
            let expected_final = 1 + 2 * (round + 1);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let mut returned = [0u64; 2];
            std::thread::scope(|scope| {
                for slot in returned.iter_mut() {
                    let runtime = runtime.clone();
                    let session = racing_session.clone();
                    let barrier = std::sync::Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        *slot = runtime
                            .advance_control_generation(&session)
                            .expect("advance")
                            .get();
                    });
                }
            });
            returned.sort_unstable();
            assert_eq!(
                returned,
                [expected_final - 1, expected_final],
                "round {round}: concurrent advances must land on consecutive generations"
            );
        }
    })
    .await
    .expect("advance threads");

    let final_generation = ControlGeneration::new(1 + 2 * ROUNDS).expect("final generation");
    for terminal in [&first_terminal, &second_terminal] {
        for older in [final_generation.get() - 1, final_generation.get() - 2] {
            let older = ControlGeneration::new(older).expect("older generation");
            assert_eq!(
                h.runtime.send(terminal, older, b"stale\n".to_vec()).await,
                Err(SendError::Rejected(SendRejection::ControlLost)),
                "every terminal coordinator must reject an older token with no byte written"
            );
        }
        h.runtime
            .send(terminal, final_generation, b"ok\n".to_vec())
            .await
            .expect("the final generation is current on every terminal");
    }

    h.runtime.stop(&first_terminal).await.expect("stop 1");
    h.runtime.stop(&second_terminal).await.expect("stop 2");
    h.runtime
        .await_cleanup(&first_terminal)
        .await
        .expect("cleanup 1");
    h.runtime
        .await_cleanup(&second_terminal)
        .await
        .expect("cleanup 2");
    h.finish().await;
}

/// Race-2 (generation loss during start vs shutdown): a spawned terminal
/// refused by a mid-start generation change must still enter the drain set,
/// so concurrent shutdown waits for its cleanup before sweeping the manager
/// cgroup root.
#[tokio::test]
async fn control_lost_start_racing_shutdown_is_drained_without_residue() {
    let Some(h) = Harness::with_limits("start-control-shutdown-race", 1, 1).await else {
        return;
    };
    let session = h.session("s1");

    h.runtime.arm_start_park();
    let starter = {
        let runtime = h.runtime.clone();
        let session = session.clone();
        let spec = h.spec(&["sleep"]);
        tokio::spawn(async move { runtime.start(&session, first(), spec).await })
    };
    h.runtime.wait_start_parked().await;
    h.runtime
        .advance_control_generation(&session)
        .expect("invalidate the start generation");

    let shutdown = {
        let runtime = h.runtime.clone();
        tokio::spawn(async move { runtime.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the refused in-flight start"
    );

    h.runtime.release_start_park();
    let start_result = starter.await.expect("starter task");
    assert!(
        matches!(start_result, Err(RuntimeError::ControlLost(_))),
        "the invalidated start must report ControlLost, got {start_result:?}"
    );
    shutdown.await.expect("shutdown task").expect("shutdown");

    let records = h.runtime.list().await.expect("list");
    let record = records
        .iter()
        .find(|snapshot| snapshot.terminal.session == session)
        .expect("the refused start's durable record exists");
    assert!(!matches!(record.process, ProcessState::Running));
    assert!(matches!(record.output, OutputState::Closed(_)));
    assert_eq!(h.runtime.occupying_slots(), 0, "quota must be released");
    assert!(!manager_root(&h.tag).exists());
    assert_eq!(terminal_cgroup_names(&manager_root(&h.tag)).len(), 0);

    let temp = h.root.0.clone();
    drop(h);
    assert!(!temp.exists(), "the temp storage root must be gone");
}

/// Race-3 (start vs shutdown): a start that passed the initial shutdown
/// check is parked at its pre-registration point. Shutdown must wait for it
/// rather than sweep; the start must be refused with `Shutdown` without ever
/// registering live and without leaving quota, cgroup, process, or durable
/// `Running` residue.
#[tokio::test]
async fn start_racing_shutdown_is_refused_without_registering_or_residue() {
    let Some(h) = Harness::with_limits("start-shutdown-race", 1, 1).await else {
        return;
    };
    let session = h.session("s1");

    // Park a start at its pre-registration point: it has passed the initial
    // shutdown check and has already spawned its terminal and cgroup.
    h.runtime.arm_start_park();
    let starter = {
        let runtime = h.runtime.clone();
        let session = session.clone();
        let spec = h.spec(&["sleep"]);
        tokio::spawn(async move { runtime.start(&session, first(), spec).await })
    };
    h.runtime.wait_start_parked().await;

    // Run shutdown while the start is parked. It must publish its flag and
    // then wait on the in-flight start, not remove the cgroup root.
    let shutdown = {
        let runtime = h.runtime.clone();
        tokio::spawn(async move { runtime.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the in-flight start, not sweep while it is parked"
    );

    h.runtime.release_start_park();
    let start_result = starter.await.expect("starter task");
    assert!(
        matches!(start_result, Err(RuntimeError::Shutdown)),
        "a start that passed its first check must be refused with Shutdown, got {start_result:?}"
    );
    shutdown.await.expect("shutdown task").expect("shutdown");

    // The late start never registered live and left no durable Running record.
    let records = h.runtime.list().await.expect("list");
    let record = records
        .iter()
        .find(|snapshot| snapshot.terminal.session == session)
        .expect("the late start's durable record exists");
    assert!(
        !matches!(record.process, ProcessState::Running),
        "the late start must not stay a Running record, got {:?}",
        record.process
    );
    assert!(
        matches!(record.output, OutputState::Closed(_)),
        "the late start's output must be closed, got {:?}",
        record.output
    );
    assert_eq!(
        h.runtime
            .send(&record.terminal, first(), b"late".to_vec())
            .await,
        Err(SendError::Rejected(SendRejection::Unknown)),
        "the late start must not be a live terminal"
    );

    // Quota released and no cgroup residue.
    assert_eq!(
        h.runtime.occupying_slots(),
        0,
        "quota slot must be released"
    );
    assert!(
        !manager_root(&h.tag).exists(),
        "the manager cgroup root must be swept"
    );
    assert_eq!(terminal_cgroup_names(&manager_root(&h.tag)).len(), 0);

    let temp = h.root.0.clone();
    drop(h);
    assert!(!temp.exists(), "the temp storage root must be gone");
}

/// Encode bytes as the lowercase hex the `emit` fixture mode decodes, so a
/// test can drive exact control sequences through a real PTY.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn position(line: u64, byte_offset: u64) -> HistoryPosition {
    HistoryPosition::new(line, byte_offset).expect("valid position")
}

#[tokio::test]
async fn read_tail_and_grep_serve_the_normalized_history() {
    let Some(h) = Harness::new("s4-query").await else {
        return;
    };
    let session = h.session("s1");
    // CJK wide characters, a CR overwrite, a combining mark, an ANSI style
    // sequence, an OSC payload, and a final line with no newline.
    let stream =
        "你好\rX\ne\u{301}t\u{4e16}a\x1b[1;31mS\x1b[0m\n\x1b]0;title\x07after-osc\nopen-tail";
    let terminal = h
        .start(&session, h.spec(&["emit", &hex(stream.as_bytes())]))
        .await;
    h.runtime.await_cleanup(&terminal).await.expect("cleanup");

    let log = h.runtime.log_identity(&terminal).await.expect("identity");
    let request = ReadRequest::first(log.clone(), None, ReadLimits::DEFAULT).expect("request");
    let result = h.runtime.read(&request).await.expect("read");
    let lines: Vec<String> = result
        .page()
        .fragments()
        .iter()
        .map(|fragment| fragment.text().to_owned())
        .collect();
    assert_eq!(
        lines,
        vec![
            "X好".to_owned(),
            "e\u{301}t\u{4e16}aS".to_owned(),
            "after-osc".to_owned(),
            "open-tail".to_owned(),
        ]
    );

    // A literal grep over the committed history binds its query and reports
    // the context lines it returned.
    let query = GrepQuery::new(log, "t世", true, position(1, 0), 4, 1).expect("query");
    let page = h
        .runtime
        .grep(&GrepRequest::fresh(query), GrepLimits::DEFAULT)
        .await
        .expect("grep");
    assert_eq!(page.matches().len(), 1);
    assert_eq!(page.matches()[0].position(), position(2, 3));
    assert_eq!(
        page.contexts().iter().map(|c| c.line()).collect::<Vec<_>>(),
        vec![1, 3]
    );

    // The tail is empty once the end of output fixed it as a history line,
    // and the history part of the view reports the retained window.
    let view = h
        .runtime
        .tail(&terminal, ReadLimits::DEFAULT)
        .await
        .expect("tail");
    assert!(view.tail().text().is_empty());
    assert!(view.history().page().retained().is_some());
    assert!(!view.history().page().degraded());

    h.finish().await;
}

#[tokio::test]
async fn tail_snapshot_reports_the_unfinished_line_while_output_is_open() {
    let Some(h) = Harness::new("s4-tail").await else {
        return;
    };
    let session = h.session("s1");
    let terminal = h
        .start(
            &session,
            h.spec(&["emit-hold", &hex("partial line".as_bytes())]),
        )
        .await;

    // The unfinished line is readable as a mutable tail (no LF was written,
    // so it is not history yet).
    let mut text = String::new();
    for _ in 0..200 {
        let view = h
            .runtime
            .tail(&terminal, ReadLimits::DEFAULT)
            .await
            .expect("tail");
        assert!(!view.tail().truncated());
        text = view.tail().text().to_owned();
        if !text.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(text, "partial line");

    h.finish().await;
}
