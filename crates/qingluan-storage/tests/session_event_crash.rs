//! Abrupt-crash matrix for the S5 session-event commit sequence, with real
//! subprocess deaths (`libc::_exit(70)`) at exact failpoints.
//!
//! `harness = false`: this binary doubles as the crash child. Invoked with
//! `--session-event-crash <root> <point>` it drives one root-exit
//! transition and dies hard at the named [`CrashPoint`]; in its test mode it
//! spawns those children, then in this (fresh) process asserts the durable
//! outcome: a crash before commit leaves no event and no state change, a
//! crash after commit leaves the committed event and never reuses its
//! sequence, and no uncommitted candidate is ever replayable. Every child
//! is waited on and every root is a temp dir removed on exit: no child,
//! file, or directory residue.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use qingluan_core::terminal::{
    ExitResult, ExternalSessionId, OutputEnd, ProcessState, SessionRef, SessionSource, TerminalId,
    TerminalRef, TerminalSize,
};
use qingluan_storage::{CrashPoint, CrashSink, LogStore, RuntimeRegistry};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s5e-crash-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }
}

impl std::ops::Deref for TempRoot {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn terminal_ref() -> TerminalRef {
    TerminalRef {
        session: SessionRef {
            source: SessionSource::new("pi"),
            external_id: ExternalSessionId::new("session-1"),
        },
        terminal_id: TerminalId::new("t1"),
    }
}

fn size() -> TerminalSize {
    TerminalSize {
        rows: 30,
        columns: 120,
    }
}

fn parse_point(name: &str) -> Option<CrashPoint> {
    Some(match name {
        "event-insert" => CrashPoint::EventInsert,
        "event-before-commit" => CrashPoint::EventBeforeCommit,
        "event-commit" => CrashPoint::EventCommit,
        "event-publish" => CrashPoint::EventPublish,
        _ => return None,
    })
}

/// The child: begin/confirm a terminal, install a sink that dies hard at the
/// target, then drive one root-exit transition (the failpoints live on its
/// single transaction and its post-commit return).
async fn crash_child(root: &Path, point: &str) -> Result<(), String> {
    let target = parse_point(point).ok_or_else(|| format!("unknown point {point}"))?;
    let store = LogStore::open(root)
        .await
        .map_err(|error| format!("open: {error}"))?;
    let registry = store.runtime_registry();
    let terminal = terminal_ref();
    registry
        .begin(&terminal, size())
        .await
        .map_err(|error| format!("begin: {error}"))?;
    registry
        .mark_running(&terminal)
        .await
        .map_err(|error| format!("mark_running: {error}"))?;
    let sink: CrashSink = Arc::new(move |hit: CrashPoint| {
        if hit == target {
            // Real abrupt death: no unwinding, no at-exit handlers, no
            // buffer flushes — the open transaction is simply gone.
            unsafe { libc::_exit(70) };
        }
    });
    store.set_crash_sink(Some(sink));
    registry
        .process_exit_committed(&terminal, ExitResult::ExitCode(0))
        .await
        .map_err(|error| format!("process_exit: {error}"))?;
    Ok(())
}

fn spawn_child(root: &Path, point: &str) {
    let exe = std::env::current_exe().expect("test binary path");
    let status = Command::new(exe)
        .args(["--session-event-crash", root.to_str().unwrap(), point])
        .status()
        .expect("spawn crash child");
    assert_eq!(
        status.code(),
        Some(70),
        "child at {point} must die abruptly with _exit(70), got {status:?}"
    );
}

/// After a pre-commit crash: no event, no state change, and the next
/// transition starts at sequence 1 (the uncommitted candidate is lost).
async fn assert_rolled_back(root: &TempRoot) {
    let store = RuntimeRegistry::open(root).await.unwrap();
    let terminal = terminal_ref();
    let record = store.load(&terminal).await.unwrap().unwrap();
    assert_eq!(
        record.process,
        ProcessState::Running,
        "pre-commit crash must roll the state change back"
    );
    let state = store.event_state(&terminal.session).await.unwrap();
    assert_eq!(state.last_committed_seq(), 0);
    let replay = store.events_after(&terminal.session, 0).await.unwrap();
    assert!(
        replay.events.is_empty(),
        "an uncommitted candidate must never be replayable"
    );

    let commit = store
        .process_exit_committed(&terminal, ExitResult::ExitCode(0))
        .await
        .unwrap();
    assert_eq!(
        commit.event.unwrap().event_seq.get(),
        1,
        "the first committed event still starts at 1"
    );
}

/// After a post-commit crash: the event is durable and replayable, and the
/// next dimension gets the following sequence (never a reused one).
async fn assert_committed(root: &TempRoot) {
    let store = RuntimeRegistry::open(root).await.unwrap();
    let terminal = terminal_ref();
    let record = store.load(&terminal).await.unwrap().unwrap();
    assert_eq!(
        record.process,
        ProcessState::Exited(ExitResult::ExitCode(0)),
        "post-commit crash must keep the committed state change"
    );
    let state = store.event_state(&terminal.session).await.unwrap();
    assert_eq!(state.last_committed_seq(), 1);
    let replay = store.events_after(&terminal.session, 0).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_seq.get(), 1);

    // The committed-but-unreturned event is not reused: a later close
    // allocation continues at 2.
    let commit = store
        .output_close_committed(&terminal, OutputEnd::Eof)
        .await
        .unwrap();
    assert_eq!(commit.event.unwrap().event_seq.get(), 2);
}

async fn case_pre_commit(name: &'static str, point: &'static str) {
    let root = TempRoot::new(name);
    spawn_child(&root, point);
    assert_rolled_back(&root).await;
}

async fn case_post_commit(name: &'static str, point: &'static str) {
    let root = TempRoot::new(name);
    spawn_child(&root, point);
    assert_committed(&root).await;
}

/// The commit-before-publish ordering, observed in one process: the event
/// insertion and the pre-commit point fire before the commit point, and the
/// publication point fires strictly after the commit point.
async fn crash_points_are_commit_before_publish() {
    let root = TempRoot::new("order");
    let store = LogStore::open(&root).await.unwrap();
    let registry = store.runtime_registry();
    let terminal = terminal_ref();
    registry.begin(&terminal, size()).await.unwrap();
    registry.mark_running(&terminal).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let sink: CrashSink = Arc::new(move |point: CrashPoint| {
        recorder.lock().unwrap().push(point);
    });
    store.set_crash_sink(Some(sink));
    registry
        .process_exit_committed(&terminal, ExitResult::ExitCode(0))
        .await
        .unwrap();

    let seen = seen.lock().unwrap().clone();
    let order: Vec<CrashPoint> = seen
        .iter()
        .copied()
        .filter(|point| {
            matches!(
                point,
                CrashPoint::EventInsert
                    | CrashPoint::EventBeforeCommit
                    | CrashPoint::EventCommit
                    | CrashPoint::EventPublish
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            CrashPoint::EventInsert,
            CrashPoint::EventBeforeCommit,
            CrashPoint::EventCommit,
            CrashPoint::EventPublish,
        ],
        "the event must be inserted, committed, and only then published"
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--session-event-crash") {
        let root = PathBuf::from(&args[2]);
        let point = args[3].clone();
        match crash_child(&root, &point).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("session-event-crash error: {error}");
                std::process::exit(2);
            }
        }
    }

    let mut failures: Vec<&'static str> = Vec::new();
    let mut run =
        |name: &'static str, fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>| {
            print!("test {name} ... ");
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            })) {
                Ok(()) => println!("ok"),
                Err(panic) => {
                    println!("FAILED ({panic:?})");
                    failures.push(name);
                }
            }
        };

    run(
        "event-insert-rolls-back",
        Box::pin(case_pre_commit("event-insert-rolls-back", "event-insert")),
    );
    run(
        "event-before-commit-rolls-back",
        Box::pin(case_pre_commit(
            "event-before-commit-rolls-back",
            "event-before-commit",
        )),
    );
    run(
        "event-commit-survives",
        Box::pin(case_post_commit("event-commit-survives", "event-commit")),
    );
    run(
        "event-publish-survives",
        Box::pin(case_post_commit("event-publish-survives", "event-publish")),
    );
    run(
        "commit-before-publish-order",
        Box::pin(crash_points_are_commit_before_publish()),
    );

    if failures.is_empty() {
        println!("SESSION-EVENT-CRASH-MATRIX-OK");
        std::process::exit(0);
    }
    eprintln!("failures: {failures:?}");
    std::process::exit(1);
}
