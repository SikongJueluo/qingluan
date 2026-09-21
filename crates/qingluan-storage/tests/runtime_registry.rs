//! S3 terminal-runtime registry tests through the public storage seam:
//! rollback, lifecycle, independent process/output dimensions, idempotence,
//! monotonic revision under concurrent dimension updates, and the atomic
//! interrupt-unfinished recovery pass (which never fabricates an exit).
//!
//! Every test uses a temp storage root deleted on drop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qingluan_core::terminal::{
    ExitResult, ExternalSessionId, OutputEnd, OutputState, ProcessState, SessionRef, SessionSource,
    TerminalId, TerminalRef, TerminalSize,
};
use qingluan_storage::{RuntimePhase, RuntimeRecord, RuntimeRegistry, StorageError};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s3-runtime-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
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

fn terminal_ref(id: &str) -> TerminalRef {
    TerminalRef {
        session: SessionRef {
            source: SessionSource::new("pi"),
            external_id: ExternalSessionId::new("session-1"),
        },
        terminal_id: TerminalId::new(id),
    }
}

fn size(rows: u16, columns: u16) -> TerminalSize {
    TerminalSize { rows, columns }
}

async fn load(store: &RuntimeRegistry, id: &str) -> RuntimeRecord {
    store.load(&terminal_ref(id)).await.unwrap().unwrap()
}

#[tokio::test]
async fn begin_reserves_a_starting_slot_and_rejects_a_duplicate() {
    let root = TempRoot::new("begin");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let terminal = terminal_ref("t1");

    let record = store.begin(&terminal, size(30, 120)).await.unwrap();
    assert_eq!(record.phase, RuntimePhase::Starting);
    assert_eq!(record.process, ProcessState::Running);
    assert_eq!(record.output, OutputState::Open);
    assert!(!record.stopping);
    assert_eq!(record.size, size(30, 120));
    assert_eq!(record.revision, 1);
    assert_eq!(record.terminal, terminal);

    assert_eq!(store.load(&terminal).await.unwrap().as_ref(), Some(&record));
    assert_eq!(store.list().await.unwrap(), vec![record]);

    // A terminal id is never reused: a second begin conflicts.
    match store.begin(&terminal, size(30, 120)).await {
        Err(StorageError::RuntimeConflict { .. }) => {}
        other => panic!("expected RuntimeConflict, got {other:?}"),
    }
    assert_eq!(store.list().await.unwrap().len(), 1);

    // A zero dimension is rejected before any row is written.
    assert!(
        store
            .begin(&terminal_ref("t2"), size(0, 120))
            .await
            .is_err()
    );
    assert!(store.load(&terminal_ref("t2")).await.unwrap().is_none());
}

#[tokio::test]
async fn running_exit_close_release_lifecycle() {
    let root = TempRoot::new("lifecycle");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let terminal = terminal_ref("t1");

    store.begin(&terminal, size(40, 100)).await.unwrap();
    let running = store.mark_running(&terminal).await.unwrap();
    assert_eq!(running.phase, RuntimePhase::Running);
    assert_eq!(running.revision, 2);
    assert_eq!(
        running.size,
        size(40, 100),
        "size is preserved across phases"
    );

    let exited = store
        .process_exit(&terminal, ExitResult::ExitCode(0))
        .await
        .unwrap();
    assert_eq!(
        exited.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    assert_eq!(exited.revision, 3);

    let closed = store.output_close(&terminal, OutputEnd::Eof).await.unwrap();
    assert_eq!(closed.output, OutputState::Closed(OutputEnd::Eof));
    assert_eq!(closed.revision, 4);

    let released = store.released(&terminal).await.unwrap();
    assert_eq!(released.phase, RuntimePhase::Released);
    assert_eq!(released.revision, 5);

    // Released is idempotent: no new revision.
    let again = store.released(&terminal).await.unwrap();
    assert_eq!(again, released);
    assert_eq!(load(&store, "t1").await.revision, 5);
}

#[tokio::test]
async fn process_and_output_dimensions_are_independent() {
    let root = TempRoot::new("dimensions");
    let store = RuntimeRegistry::open(&root).await.unwrap();

    // Exit before the output closes: process Exited, output still Open.
    let early_exit = terminal_ref("early-exit");
    store.begin(&early_exit, size(30, 120)).await.unwrap();
    store.mark_running(&early_exit).await.unwrap();
    let record = store
        .process_exit(&early_exit, ExitResult::ExitCode(3))
        .await
        .unwrap();
    assert_eq!(
        record.process,
        ProcessState::Exited(ExitResult::ExitCode(3))
    );
    assert_eq!(record.output, OutputState::Open);
    let record = store
        .output_close(&early_exit, OutputEnd::Eof)
        .await
        .unwrap();
    assert_eq!(record.output, OutputState::Closed(OutputEnd::Eof));
    assert_eq!(
        record.process,
        ProcessState::Exited(ExitResult::ExitCode(3))
    );

    // Output closes while the process still runs: output Closed, process
    // still Running (no ordering between the two dimensions).
    let early_close = terminal_ref("early-close");
    store.begin(&early_close, size(30, 120)).await.unwrap();
    store.mark_running(&early_close).await.unwrap();
    let record = store
        .output_close(&early_close, OutputEnd::ForcedClose)
        .await
        .unwrap();
    assert_eq!(record.output, OutputState::Closed(OutputEnd::ForcedClose));
    assert_eq!(record.process, ProcessState::Running);
    let record = store
        .process_exit(&early_close, ExitResult::Signal(9))
        .await
        .unwrap();
    assert_eq!(record.process, ProcessState::Exited(ExitResult::Signal(9)));
    assert_eq!(record.output, OutputState::Closed(OutputEnd::ForcedClose));
}

#[tokio::test]
async fn rollback_starting_frees_the_slot_idempotently() {
    let root = TempRoot::new("rollback");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let terminal = terminal_ref("t1");

    store.begin(&terminal, size(30, 120)).await.unwrap();
    let rolled_back = store.rollback_starting(&terminal).await.unwrap();
    assert_eq!(rolled_back.phase, RuntimePhase::Released);
    assert_eq!(rolled_back.revision, 2);
    // Repeating the rollback is a no-op.
    assert_eq!(
        store.rollback_starting(&terminal).await.unwrap(),
        rolled_back
    );

    // After a confirmed start, rollback refuses: the terminal must be
    // stopped and reclaimed instead.
    let started = terminal_ref("t2");
    store.begin(&started, size(30, 120)).await.unwrap();
    store.mark_running(&started).await.unwrap();
    match store.rollback_starting(&started).await {
        Err(StorageError::RuntimeConflict { .. }) => {}
        other => panic!("expected RuntimeConflict, got {other:?}"),
    }
    assert_eq!(load(&store, "t2").await.phase, RuntimePhase::Running);
}

#[tokio::test]
async fn stop_intent_enters_cleaning_and_is_idempotent() {
    let root = TempRoot::new("stop");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let terminal = terminal_ref("t1");

    store.begin(&terminal, size(30, 120)).await.unwrap();
    let stopping = store.stop_intent(&terminal).await.unwrap();
    assert_eq!(stopping.phase, RuntimePhase::Cleaning);
    assert!(stopping.stopping);
    assert_eq!(stopping.revision, 2);
    // A second stop intent shares the first: one commit, no new revision.
    assert_eq!(store.stop_intent(&terminal).await.unwrap(), stopping);

    // The slot stays occupied until released.
    assert_eq!(load(&store, "t1").await.phase, RuntimePhase::Cleaning);
    let released = store.released(&terminal).await.unwrap();
    assert_eq!(released.phase, RuntimePhase::Released);
    assert!(released.stopping);

    // Stop intent after release conflicts.
    match store.stop_intent(&terminal).await {
        Err(StorageError::RuntimeConflict { .. }) => {}
        other => panic!("expected RuntimeConflict, got {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_dimension_updates_keep_revision_monotonic() {
    let root = TempRoot::new("concurrent");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let terminal = terminal_ref("t1");
    store.begin(&terminal, size(30, 120)).await.unwrap();

    let exit_store = store.clone();
    let exit_terminal = terminal.clone();
    let close_store = store.clone();
    let close_terminal = terminal.clone();
    let exit = tokio::spawn(async move {
        exit_store
            .process_exit(&exit_terminal, ExitResult::ExitCode(7))
            .await
    });
    let close = tokio::spawn(async move {
        close_store
            .output_close(&close_terminal, OutputEnd::Eof)
            .await
    });
    exit.await.unwrap().unwrap();
    close.await.unwrap().unwrap();

    // Both independent dimensions landed and each change advanced the
    // revision exactly once (begin = 1, then +1 +1); never a lost update.
    let final_record = load(&store, "t1").await;
    assert_eq!(
        final_record.process,
        ProcessState::Exited(ExitResult::ExitCode(7))
    );
    assert_eq!(final_record.output, OutputState::Closed(OutputEnd::Eof));
    assert_eq!(final_record.revision, 3);
}

#[tokio::test]
async fn interrupt_unfinished_marks_only_unfinished_and_never_fabricates() {
    let root = TempRoot::new("interrupt");
    let store = RuntimeRegistry::open(&root).await.unwrap();

    // t1: just reserved.
    let reserved = terminal_ref("t1");
    store.begin(&reserved, size(30, 120)).await.unwrap();
    // t2: a known exit was already recorded (must be preserved).
    let exited = terminal_ref("t2");
    store.begin(&exited, size(30, 120)).await.unwrap();
    store.mark_running(&exited).await.unwrap();
    store
        .process_exit(&exited, ExitResult::ExitCode(0))
        .await
        .unwrap();
    // t3: fully released (must stay untouched).
    let released = terminal_ref("t3");
    store.begin(&released, size(30, 120)).await.unwrap();
    store.released(&released).await.unwrap();
    let released_before = load(&store, "t3").await;
    // t4: output already ended normally while the process still runs.
    let closed_output = terminal_ref("t4");
    store.begin(&closed_output, size(30, 120)).await.unwrap();
    store
        .output_close(&closed_output, OutputEnd::Eof)
        .await
        .unwrap();

    let changed = store.interrupt_unfinished().await.unwrap();
    let changed_ids: Vec<&str> = changed
        .iter()
        .map(|record| record.terminal.terminal_id.as_str())
        .collect();
    assert_eq!(changed_ids, vec!["t1", "t2", "t4"]);

    let t1 = load(&store, "t1").await;
    assert_eq!(t1.phase, RuntimePhase::Cleaning);
    assert_eq!(t1.process, ProcessState::Interrupted);
    assert_eq!(t1.output, OutputState::Closed(OutputEnd::Interrupted));
    assert_eq!(t1.revision, 2, "recovery bumps revision exactly once");

    let t2 = load(&store, "t2").await;
    assert_eq!(
        t2.process,
        ProcessState::Exited(ExitResult::ExitCode(0)),
        "a known exit is never overwritten by Interrupted"
    );
    assert_eq!(t2.output, OutputState::Closed(OutputEnd::Interrupted));
    assert_eq!(t2.phase, RuntimePhase::Cleaning);

    let t4 = load(&store, "t4").await;
    assert_eq!(t4.process, ProcessState::Interrupted);
    assert_eq!(
        t4.output,
        OutputState::Closed(OutputEnd::Eof),
        "a known output end is never overwritten by Interrupted"
    );

    assert_eq!(
        load(&store, "t3").await,
        released_before,
        "a released record is never reopened"
    );

    // Idempotent: the second pass changes nothing, so consecutive
    // recoveries observe an equal durable state.
    let snapshot: Vec<RuntimeRecord> = store.list().await.unwrap();
    assert!(store.interrupt_unfinished().await.unwrap().is_empty());
    assert_eq!(store.list().await.unwrap(), snapshot);
}

#[tokio::test]
async fn load_unknown_returns_none_and_list_orders_by_creation() {
    let root = TempRoot::new("list");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    assert!(
        store
            .load(&terminal_ref("missing"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.list().await.unwrap().is_empty());

    store
        .begin(&terminal_ref("b"), size(30, 120))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store
        .begin(&terminal_ref("a"), size(30, 120))
        .await
        .unwrap();
    let ids: Vec<String> = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.terminal.terminal_id.as_str().to_owned())
        .collect();
    assert_eq!(ids, vec!["b", "a"]);
}
