//! S5 durable session lifecycle event tests through the public storage
//! seam: same-transaction state+event commit, duplicate/conflict behavior,
//! both lifecycle orders, monotonic bounded ack, explicit contiguous prune,
//! bounded replay clamped to the committed cut, stale-range refusal,
//! restart persistence, session isolation, corrupt-row refusal, and event
//! survivability after the terminal record is deleted.
//!
//! Every test uses a temp storage root deleted on drop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qingluan_core::terminal::{
    ExitResult, ExternalSessionId, OutputEnd, ProcessState, SessionEvent, SessionEventPayload,
    SessionEventState, SessionRef, SessionSource, TerminalId, TerminalRef, TerminalSize,
};
use qingluan_storage::{
    EventReplay, LifecycleCommit, MAX_EVENT_PAGE, RuntimeRegistry, StorageError,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s5-events-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("terminal.db")
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

fn session(source: &str, id: &str) -> SessionRef {
    SessionRef {
        source: SessionSource::new(source),
        external_id: ExternalSessionId::new(id),
    }
}

fn terminal(source: &str, session_id: &str, terminal_id: &str) -> TerminalRef {
    TerminalRef {
        session: session(source, session_id),
        terminal_id: TerminalId::new(terminal_id),
    }
}

fn size(rows: u16, columns: u16) -> TerminalSize {
    TerminalSize { rows, columns }
}

async fn raw_pool(root: &TempRoot) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.db());
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// Reserve, confirm, and record a root exit for a fresh terminal, returning
/// the committed event.
async fn exit_new_terminal(
    store: &RuntimeRegistry,
    terminal: &TerminalRef,
    result: ExitResult,
) -> qingluan_core::terminal::SessionEvent {
    store.begin(terminal, size(30, 120)).await.unwrap();
    store.mark_running(terminal).await.unwrap();
    store
        .process_exit_committed(terminal, result)
        .await
        .unwrap()
        .event
        .expect("a first transition commits exactly one event")
}

/// Compile-time guard: the storage API speaks only in qingluan-core domain
/// types (no sqlx/wire type crosses it).
#[allow(dead_code)]
fn type_guard(
    replay: EventReplay,
    commit: LifecycleCommit,
    state: SessionEventState,
    event: SessionEvent,
) -> (
    EventReplay,
    LifecycleCommit,
    SessionEventState,
    SessionEvent,
) {
    (replay, commit, state, event)
}

#[tokio::test]
async fn both_lifecycle_orders_commit_one_event_each() {
    let root = TempRoot::new("orders");
    let store = RuntimeRegistry::open(&root).await.unwrap();

    // Exit first, then output close.
    let t1 = terminal("pi", "s-exit-first", "t1");
    store.begin(&t1, size(30, 120)).await.unwrap();
    store.mark_running(&t1).await.unwrap();
    let exit = store
        .process_exit_committed(&t1, ExitResult::ExitCode(0))
        .await
        .unwrap();
    assert_eq!(
        exit.record.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    let event = exit.event.expect("exit commits an event");
    assert_eq!(event.terminal, t1);
    assert_eq!(event.event_seq.get(), 1);
    assert_eq!(
        event.payload,
        SessionEventPayload::ProcessExited(ExitResult::ExitCode(0))
    );

    let close = store
        .output_close_committed(&t1, OutputEnd::Eof)
        .await
        .unwrap();
    let event = close.event.expect("close commits an event");
    assert_eq!(event.event_seq.get(), 2);
    assert_eq!(
        event.payload,
        SessionEventPayload::OutputClosed(OutputEnd::Eof)
    );

    // Close first, then exit, in a different session: the two dimensions
    // have no ordering, and each still commits exactly one event.
    let t2 = terminal("pi", "s-close-first", "t1");
    store.begin(&t2, size(30, 120)).await.unwrap();
    store.mark_running(&t2).await.unwrap();
    let close = store
        .output_close_committed(&t2, OutputEnd::ForcedClose)
        .await
        .unwrap();
    assert_eq!(
        close.record.output,
        qingluan_core::terminal::OutputState::Closed(OutputEnd::ForcedClose)
    );
    assert_eq!(
        close.event.expect("close commits an event").event_seq.get(),
        1
    );
    let exit = store
        .process_exit_committed(&t2, ExitResult::Signal(15))
        .await
        .unwrap();
    assert_eq!(
        exit.event.expect("exit commits an event").event_seq.get(),
        2
    );

    let state = store.event_state(&t1.session).await.unwrap();
    assert_eq!(
        (
            state.pruned_through_seq(),
            state.acked_through_seq(),
            state.last_committed_seq()
        ),
        (0, 0, 2)
    );
    let replay = store.events_after(&t1.session, 0).await.unwrap();
    assert_eq!(replay.events.len(), 2);
    assert_eq!(replay.next_after_seq, None);
}

#[tokio::test]
async fn duplicate_transitions_are_idempotent_and_conflicts_stay_typed() {
    let root = TempRoot::new("idempotent");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let t1 = terminal("pi", "s1", "t1");
    let first = exit_new_terminal(&store, &t1, ExitResult::ExitCode(7)).await;
    assert_eq!(first.event_seq.get(), 1);

    // A repeated equal exit returns the same record and allocates no event.
    let again = store
        .process_exit_committed(&t1, ExitResult::ExitCode(7))
        .await
        .unwrap();
    assert!(again.event.is_none());
    assert_eq!(
        again.record.process,
        ProcessState::Exited(ExitResult::ExitCode(7))
    );
    // The plain caller-facing method is idempotent too and still writes no
    // duplicate (the terminal runtime uses this path).
    assert_eq!(
        store
            .process_exit(&t1, ExitResult::ExitCode(7))
            .await
            .unwrap(),
        again.record
    );

    // A conflicting exit is a typed failure and allocates no event.
    match store
        .process_exit_committed(&t1, ExitResult::ExitCode(9))
        .await
    {
        Err(StorageError::RuntimeConflict { .. }) => {}
        other => panic!("expected RuntimeConflict, got {other:?}"),
    }

    // A repeated equal close is idempotent as well.
    let close = store
        .output_close_committed(&t1, OutputEnd::ReadError)
        .await
        .unwrap();
    assert_eq!(close.event.expect("first close commits").event_seq.get(), 2);
    let close_again = store
        .output_close_committed(&t1, OutputEnd::ReadError)
        .await
        .unwrap();
    assert!(close_again.event.is_none());
    match store.output_close_committed(&t1, OutputEnd::Eof).await {
        Err(StorageError::RuntimeConflict { .. }) => {}
        other => panic!("expected RuntimeConflict, got {other:?}"),
    }

    let state = store.event_state(&t1.session).await.unwrap();
    assert_eq!(state.last_committed_seq(), 2);
    let replay = store.events_after(&t1.session, 0).await.unwrap();
    assert_eq!(replay.events.len(), 2);
}

#[tokio::test]
async fn plain_process_exit_persists_the_event_without_publishing() {
    let root = TempRoot::new("plain-path");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let t1 = terminal("pi", "s1", "t1");
    store.begin(&t1, size(30, 120)).await.unwrap();
    store.mark_running(&t1).await.unwrap();

    // The terminal runtime's plain call returns only the record, but the
    // event is committed in the same transaction and therefore replayable.
    let record = store
        .process_exit(&t1, ExitResult::ExitCode(0))
        .await
        .unwrap();
    assert_eq!(
        record.process,
        ProcessState::Exited(ExitResult::ExitCode(0))
    );
    let replay = store.events_after(&t1.session, 0).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_seq.get(), 1);
}

#[tokio::test]
async fn ack_is_monotonic_and_rejects_beyond_committed() {
    let root = TempRoot::new("ack");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-ack");
    for (i, result) in [
        ExitResult::ExitCode(0),
        ExitResult::Signal(9),
        ExitResult::ExitCode(3),
    ]
    .into_iter()
    .enumerate()
    {
        let t = terminal("pi", "s-ack", &format!("t{i}"));
        exit_new_terminal(&store, &t, result).await;
    }

    // Forward ack advances the bound.
    let state = store.ack_events(&s, 2).await.unwrap();
    assert_eq!(
        (state.acked_through_seq(), state.last_committed_seq()),
        (2, 3)
    );
    // A lower ack is a harmless no-op (never regresses).
    let state = store.ack_events(&s, 1).await.unwrap();
    assert_eq!(state.acked_through_seq(), 2);
    // Acking the exact committed bound is allowed.
    let state = store.ack_events(&s, 3).await.unwrap();
    assert_eq!(state.acked_through_seq(), 3);
    // Beyond the committed bound is refused and changes nothing.
    match store.ack_events(&s, 4).await {
        Err(StorageError::EventAckOutOfBounds { acked, committed }) => {
            assert_eq!((acked, committed), (4, 3));
        }
        other => panic!("expected EventAckOutOfBounds, got {other:?}"),
    }
    let state = store.event_state(&s).await.unwrap();
    assert_eq!(state.acked_through_seq(), 3);
}

#[tokio::test]
async fn prune_is_explicit_contiguous_and_sequence_continues_after_full_prune() {
    let root = TempRoot::new("prune");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-prune");
    for i in 0..3 {
        let t = terminal("pi", "s-prune", &format!("t{i}"));
        exit_new_terminal(&store, &t, ExitResult::ExitCode(i)).await;
    }

    // Prune beyond the acked bound is refused and deletes nothing.
    match store.prune_events(&s, 1).await {
        Err(StorageError::EventPruneOutOfBounds { through, acked }) => {
            assert_eq!((through, acked), (1, 0));
        }
        other => panic!("expected EventPruneOutOfBounds, got {other:?}"),
    }
    assert_eq!(store.events_after(&s, 0).await.unwrap().events.len(), 3);

    // Ack 2 then prune the explicit prefix 2: only seqs 1..=2 are deleted
    // and pruned advances while acked/committed are untouched.
    store.ack_events(&s, 2).await.unwrap();
    let state = store.prune_events(&s, 2).await.unwrap();
    assert_eq!(
        (
            state.pruned_through_seq(),
            state.acked_through_seq(),
            state.last_committed_seq()
        ),
        (2, 2, 3)
    );
    let replay = store.events_after(&s, 2).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_seq.get(), 3);

    // A repeated prune is harmless and never lowers the watermarks.
    let state = store.prune_events(&s, 2).await.unwrap();
    assert_eq!(state.pruned_through_seq(), 2);

    // Full prune empties the table; a new event must continue at 4, never
    // restart at 1 (the MAX(event_seq) regression the probe guards).
    store.ack_events(&s, 3).await.unwrap();
    let state = store.prune_events(&s, 3).await.unwrap();
    assert_eq!(state.pruned_through_seq(), 3);
    assert_eq!(state.last_committed_seq(), 3);

    let t = terminal("pi", "s-prune", "t-next");
    let event = exit_new_terminal(&store, &t, ExitResult::ExitCode(42)).await;
    assert_eq!(event.event_seq.get(), 4, "sequence must not be reused");
    let state = store.event_state(&s).await.unwrap();
    assert_eq!(
        (
            state.pruned_through_seq(),
            state.acked_through_seq(),
            state.last_committed_seq()
        ),
        (3, 3, 4)
    );
}

#[tokio::test]
async fn stale_replay_after_prune_is_refused_with_recovery_bound() {
    let root = TempRoot::new("stale");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-stale");
    for i in 0..4 {
        let t = terminal("pi", "s-stale", &format!("t{i}"));
        exit_new_terminal(&store, &t, ExitResult::ExitCode(i)).await;
    }
    store.ack_events(&s, 3).await.unwrap();
    store.prune_events(&s, 3).await.unwrap();

    // A request strictly before the pruned bound must not silently jump.
    match store.events_after(&s, 1).await {
        Err(StorageError::EventRangeCleared {
            after,
            pruned_through_seq,
            available_after_seq,
        }) => {
            assert_eq!(after, 1);
            assert_eq!(pruned_through_seq, 3);
            assert_eq!(available_after_seq, 3);
        }
        other => panic!("expected EventRangeCleared, got {other:?}"),
    }
    // Resuming at the pruned bound is fine and returns the retained tail.
    let replay = store.events_after(&s, 3).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_seq.get(), 4);
    // A request exactly at the pruned bound over a fully pruned session is
    // an empty page, not an error.
    store.ack_events(&s, 4).await.unwrap();
    store.prune_events(&s, 4).await.unwrap();
    let replay = store.events_after(&s, 4).await.unwrap();
    assert!(replay.events.is_empty());
    assert_eq!(replay.next_after_seq, None);
}

#[tokio::test]
async fn replay_pages_are_capped_and_clamped_to_the_committed_cut() {
    let root = TempRoot::new("paging");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-page");

    // Seed more committed events than one page holds, directly (the paging
    // logic is what is under test, not the allocator).
    let total = (MAX_EVENT_PAGE + 10) as i64;
    let pool = raw_pool(&root).await;
    for seq in 1..=total {
        sqlx::query(
            "INSERT INTO session_event
                 (session_source, external_session_id, terminal_id, event_seq, kind,
                  exit_kind, exit_value, output_end, created_ms)
             VALUES ('pi', 's-page', 't', ?1, 'exited', 'code', 0, NULL, 0)",
        )
        .bind(seq)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO session_state
             (session_source, external_session_id, pruned_through_seq,
              acked_through_seq, last_committed_seq)
         VALUES ('pi', 's-page', 0, 0, ?1)",
    )
    .bind(total)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let page1 = store.events_after(&s, 0).await.unwrap();
    assert_eq!(page1.events.len(), MAX_EVENT_PAGE);
    assert_eq!(page1.events.first().unwrap().event_seq.get(), 1);
    assert_eq!(
        page1.events.last().unwrap().event_seq.get(),
        MAX_EVENT_PAGE as u64
    );
    assert_eq!(page1.state.last_committed_seq(), total as u64);
    let next = page1.next_after_seq.expect("continuation metadata");

    // A committed event appended after page one does not extend it.
    let pool = raw_pool(&root).await;
    sqlx::query(
        "INSERT INTO session_event
             (session_source, external_session_id, terminal_id, event_seq, kind,
              exit_kind, exit_value, output_end, created_ms)
         VALUES ('pi', 's-page', 't', ?1, 'output_closed', NULL, NULL, 'eof', 0)",
    )
    .bind(total + 1)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE session_state SET last_committed_seq = ?1
          WHERE session_source = 'pi' AND external_session_id = 's-page'",
    )
    .bind(total + 1)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    // The already-returned page is unchanged; the continuation reaches the
    // rest only through the snapshot taken this call.
    assert!(
        !page1
            .events
            .iter()
            .any(|event| event.event_seq.get() > MAX_EVENT_PAGE as u64)
    );
    let page2 = store.events_after(&s, next).await.unwrap();
    assert_eq!(page2.events.len(), 11);
    assert_eq!(page2.events.first().unwrap().event_seq.get(), next + 1);
    assert_eq!(
        page2.events.last().unwrap().event_seq.get(),
        total as u64 + 1
    );
    assert_eq!(page2.next_after_seq, None);
}

#[tokio::test]
async fn watermarks_and_unpruned_events_survive_restart() {
    let root = TempRoot::new("restart");
    {
        let store = RuntimeRegistry::open(&root).await.unwrap();
        let s = session("pi", "s-restart");
        for i in 0..2 {
            let t = terminal("pi", "s-restart", &format!("t{i}"));
            exit_new_terminal(&store, &t, ExitResult::ExitCode(i)).await;
        }
        store.ack_events(&s, 1).await.unwrap();
        store.prune_events(&s, 1).await.unwrap();
    }

    // A fresh process/reopen observes the same durable watermarks and the
    // unpruned tail.
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-restart");
    let state = store.event_state(&s).await.unwrap();
    assert_eq!(
        (
            state.pruned_through_seq(),
            state.acked_through_seq(),
            state.last_committed_seq()
        ),
        (1, 1, 2)
    );
    let replay = store.events_after(&s, 1).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].event_seq.get(), 2);

    // Numbering continues past the pruned prefix across the restart.
    let t = terminal("pi", "s-restart", "t-after");
    let event = exit_new_terminal(&store, &t, ExitResult::ExitCode(8)).await;
    assert_eq!(event.event_seq.get(), 3);
}

#[tokio::test]
async fn sessions_are_isolated() {
    let root = TempRoot::new("isolation");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let a = session("pi", "s-a");
    let b = session("pi", "s-b");

    exit_new_terminal(
        &store,
        &terminal("pi", "s-a", "t1"),
        ExitResult::ExitCode(0),
    )
    .await;
    exit_new_terminal(
        &store,
        &terminal("pi", "s-b", "t1"),
        ExitResult::ExitCode(0),
    )
    .await;
    exit_new_terminal(
        &store,
        &terminal("pi", "s-a", "t2"),
        ExitResult::ExitCode(1),
    )
    .await;

    let state_a = store.event_state(&a).await.unwrap();
    let state_b = store.event_state(&b).await.unwrap();
    assert_eq!(state_a.last_committed_seq(), 2);
    assert_eq!(state_b.last_committed_seq(), 1);
    assert_eq!(store.events_after(&a, 0).await.unwrap().events.len(), 2);
    assert_eq!(store.events_after(&b, 0).await.unwrap().events.len(), 1);

    // A different source is a different session even with the same id.
    let other = session("claude", "s-a");
    assert_eq!(
        store
            .event_state(&other)
            .await
            .unwrap()
            .last_committed_seq(),
        0
    );
    // Ack/prune on one session never touches the other.
    store.ack_events(&a, 2).await.unwrap();
    store.prune_events(&a, 2).await.unwrap();
    assert_eq!(store.event_state(&b).await.unwrap().pruned_through_seq(), 0);
}

#[tokio::test]
async fn missing_session_is_all_zero_and_never_fabricates_rows() {
    let root = TempRoot::new("missing");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let absent = session("pi", "s-absent");

    let state = store.event_state(&absent).await.unwrap();
    assert_eq!(
        (
            state.pruned_through_seq(),
            state.acked_through_seq(),
            state.last_committed_seq()
        ),
        (0, 0, 0)
    );
    let replay = store.events_after(&absent, 0).await.unwrap();
    assert!(replay.events.is_empty());
    assert_eq!(replay.next_after_seq, None);
    assert_eq!(replay.state.last_committed_seq(), 0);

    // Mutations on a nonexistent session are coherent no-ops, not
    // fabricated rows.
    let acked = store.ack_events(&absent, 0).await.unwrap();
    assert_eq!(acked.acked_through_seq(), 0);
    let pruned = store.prune_events(&absent, 0).await.unwrap();
    assert_eq!(pruned.pruned_through_seq(), 0);
    // An ack beyond the (zero) committed bound is still refused.
    assert!(matches!(
        store.ack_events(&absent, 1).await,
        Err(StorageError::EventAckOutOfBounds { .. })
    ));

    let pool = raw_pool(&root).await;
    let state_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    let event_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_event")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(state_rows, 0, "no state row may be fabricated");
    assert_eq!(event_rows, 0);
}

#[tokio::test]
async fn ensure_event_session_explicitly_creates_one_idempotent_zero_state() {
    let root = TempRoot::new("ensure-session");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let session = session("pi", "s-ensure");

    for _ in 0..2 {
        let state = store.ensure_event_session(&session).await.unwrap();
        assert_eq!(
            (
                state.pruned_through_seq(),
                state.acked_through_seq(),
                state.last_committed_seq()
            ),
            (0, 0, 0)
        );
    }

    let pool = raw_pool(&root).await;
    let state_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_state
         WHERE session_source = 'pi' AND external_session_id = 's-ensure'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let event_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_event")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(state_rows, 1);
    assert_eq!(event_rows, 0);
}

#[tokio::test]
async fn corrupt_event_row_decoding_is_refused() {
    let root = TempRoot::new("corrupt");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let s = session("pi", "s-corrupt");
    let pool = raw_pool(&root).await;
    // A row the decoder cannot represent: an exit value outside the i32
    // domain. The schema CHECK does not bound the value, so this is a
    // decode-time refusal.
    sqlx::query(
        "INSERT INTO session_event
             (session_source, external_session_id, terminal_id, event_seq, kind,
              exit_kind, exit_value, output_end, created_ms)
         VALUES ('pi', 's-corrupt', 't', 1, 'exited', 'code', 5000000000, NULL, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO session_state
             (session_source, external_session_id, pruned_through_seq,
              acked_through_seq, last_committed_seq)
         VALUES ('pi', 's-corrupt', 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    match store.events_after(&s, 0).await {
        Err(StorageError::Database(_)) => {}
        other => panic!("expected a typed decode refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn events_survive_the_terminal_record_being_deleted() {
    let root = TempRoot::new("survives-delete");
    let store = RuntimeRegistry::open(&root).await.unwrap();
    let t1 = terminal("pi", "s1", "t1");
    let event = exit_new_terminal(&store, &t1, ExitResult::Signal(11)).await;
    assert_eq!(event.event_seq.get(), 1);

    // Delete the terminal's runtime record directly (S9 cleanup is out of
    // scope here): event replay must still work and stay self-explanatory.
    let pool = raw_pool(&root).await;
    sqlx::query(
        "DELETE FROM terminal_runtime
          WHERE session_source = 'pi' AND external_session_id = 's1' AND terminal_id = 't1'",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    assert!(store.load(&t1).await.unwrap().is_none());

    let replay = store.events_after(&t1.session, 0).await.unwrap();
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].terminal, t1);
    assert_eq!(
        replay.events[0].payload,
        SessionEventPayload::ProcessExited(ExitResult::Signal(11))
    );
    assert_eq!(
        store
            .event_state(&t1.session)
            .await
            .unwrap()
            .last_committed_seq(),
        1
    );
}
