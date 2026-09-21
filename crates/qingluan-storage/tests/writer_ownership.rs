//! S2 final-review blocker tests through the public seam: exclusive
//! writer ownership (one writer per log across handles and processes,
//! crash-released), the commit compare-and-set, post-extraction failure
//! latching (no silent range skip), per-stream flush/driver outcomes
//! (committed vs dropped) on both unsafe-reclaim paths, and graceful
//! `close` of the flush driver parked at write/sync/commit boundaries.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qingluan_core::terminal::{
    ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TerminalId, TerminalRef,
};
use qingluan_storage::{
    FLUSH_MAX_BYTES, FLUSH_MAX_DELAY, FaultSite, LogStore, LogStream, LogWriter,
    MAX_SEGMENT_METADATA_ROWS, ParkPoint, RecoveryAction, StorageError, StreamFlushOutcome,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s2-own-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
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

const TERMINAL: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const EPOCH: &str = "0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d";

fn log_identity() -> LogIdentity {
    LogIdentity {
        terminal: TerminalRef {
            session: SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("session-1"),
            },
            terminal_id: TerminalId::new(TERMINAL),
        },
        log_epoch: LogEpoch::new(EPOCH),
    }
}

async fn raw_pool(root: &Path) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.join("terminal.db"));
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// Drive one log to the metadata budget with a tiny rotation threshold,
/// so every further segment creation must reclaim (or be refused).
async fn saturate_budget(store: &LogStore, log: &LogIdentity) {
    let mut writer = store.open_writer(log).await.unwrap();
    writer.set_rotation_threshold(2048).await;
    let text = "l-".to_owned() + &"x".repeat(1400);
    let mut line = 0u64;
    loop {
        line += 1;
        writer.append_line(line, &text).await.unwrap();
        writer.flush().await.unwrap();
        if store.recovery_snapshot(log).await.unwrap().segments.len() == MAX_SEGMENT_METADATA_ROWS {
            break;
        }
    }
    writer.close().await.unwrap();
}

/// Tamper the live rows into an unsafe shape: nothing is sealed, so the
/// next creation has nothing it may reclaim and the batch must be
/// dropped as an explicit gap instead of failing the producer.
async fn make_reclaim_unsafe(root: &Path) {
    let pool = raw_pool(root).await;
    sqlx::query("UPDATE segment SET state = 'active'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn one_writer_per_log_across_handles_until_close() {
    let root = TempRoot::new("exclusive");
    let store = LogStore::open(&root).await.unwrap();
    let other = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();

    // A second handle — and recovery — refuse while the writer is
    // attached: two writers would attach one watermark and overlap.
    for label in ["other-handle", "same-handle"] {
        let attempt = match label {
            "other-handle" => other.open_writer(&log).await,
            _ => store.open_writer(&log).await,
        };
        assert!(
            matches!(attempt, Err(StorageError::WriterAlreadyActive { .. })),
            "{label}: second attach must refuse"
        );
    }
    assert!(matches!(
        store.recover(&log).await,
        Err(StorageError::WriterAlreadyActive { .. })
    ));

    // The attached writer keeps both streams; a graceful close releases
    // the lease deterministically and reports the final outcomes.
    writer.append_line(1, "one").await.unwrap();
    writer.append_raw(b"raw-one").await.unwrap();
    let outcomes = writer.close().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Committed);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Committed);

    // A fresh writer (other handle) continues both streams' numbering.
    let mut next = other.open_writer(&log).await.unwrap();
    assert_eq!(next.line_watermark(), 1);
    assert_eq!(next.raw_watermark(), 7);
    next.append_line(2, "two").await.unwrap();
    next.append_raw(b"raw-two").await.unwrap();
    next.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-oneraw-two".to_vec()
    );
    next.close().await.unwrap();
}

/// Racing attaches: exactly one writer wins, every loser gets the typed
/// refusal (both streams belong to that one winner afterwards).
#[tokio::test]
async fn concurrent_attaches_elect_exactly_one_writer() {
    let root = TempRoot::new("race");
    let store = std::sync::Arc::new(LogStore::open(&root).await.unwrap());
    let log = log_identity();
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let store = std::sync::Arc::clone(&store);
        let log = log.clone();
        tasks.push(tokio::spawn(async move { store.open_writer(&log).await }));
    }
    let mut winner = None;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(writer) => {
                assert!(winner.is_none(), "two writers attached at once");
                winner = Some(writer);
            }
            Err(StorageError::WriterAlreadyActive { .. }) => refused += 1,
            other => panic!("unexpected attach result: {:?}", other.map(|_| ())),
        }
    }
    assert!(winner.is_some(), "exactly one writer may attach");
    assert_eq!(refused, 2);
    winner.take().unwrap().close().await.unwrap();
}

/// The commit's compare-and-set: when the durable watermark moved since
/// this writer cached it, the commit refuses as a typed conflict (never
/// overlapping ranges behind the monotonic MAX), and the stream latches
/// recovery-required.
#[tokio::test]
async fn commit_refuses_when_the_durable_watermark_moved() {
    for stream in [LogStream::Normalized, LogStream::Raw] {
        let root = TempRoot::new("cas");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        match stream {
            LogStream::Normalized => {
                writer.append_line(1, "one").await.unwrap();
                writer.flush().await.unwrap();
            }
            LogStream::Raw => {
                writer.append_raw(b"one").await.unwrap();
                writer.flush().await.unwrap();
            }
        }
        // Simulate a second writer's commit behind this one's back.
        let pool = raw_pool(&root).await;
        match stream {
            LogStream::Normalized => {
                sqlx::query("UPDATE terminal SET line_watermark = 40")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            LogStream::Raw => {
                sqlx::query("UPDATE terminal SET raw_watermark = 40")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        pool.close().await;

        match stream {
            LogStream::Normalized => {
                writer.append_line(2, "two").await.unwrap();
                assert!(matches!(
                    writer.flush().await,
                    Err(StorageError::CommitConflict { .. })
                ));
            }
            LogStream::Raw => {
                writer.append_raw(b"two").await.unwrap();
                assert!(matches!(
                    writer.flush().await,
                    Err(StorageError::CommitConflict { .. })
                ));
            }
        }
        // The refusing stream latches recovery-required; the other stream
        // of the same writer is unaffected.
        match stream {
            LogStream::Normalized => {
                assert!(matches!(
                    writer.append_line(2, "retry").await,
                    Err(StorageError::RecoveryRequired { .. })
                ));
                writer.append_raw(b"raw-continues").await.unwrap();
                let _ = writer.flush().await;
            }
            LogStream::Raw => {
                assert!(matches!(
                    writer.append_raw(b"retry").await,
                    Err(StorageError::RecoveryRequired { .. })
                ));
                writer.append_line(1, "norm-continues").await.unwrap();
                let _ = writer.flush().await;
            }
        }
        // Recovery converges: the uncommitted tail is quarantined back to
        // the committed boundary. The tampered watermark never decreases,
        // so the range it consumed without a segment becomes an explicit
        // hole gap (never fabricated continuity), and numbering continues
        // past it without reuse.
        writer.close().await.unwrap_err();
        let report = store.recover(&log).await.unwrap();
        assert!(
            report
                .actions
                .iter()
                .any(|action| matches!(action, RecoveryAction::TailQuarantined { .. })),
            "{stream:?}: {:?}",
            report.actions
        );
        let snapshot = store.recovery_snapshot(&log).await.unwrap();
        assert!(snapshot.degraded, "{stream:?}");
        assert_eq!(snapshot.gaps.len(), 1, "{stream:?}: {:?}", snapshot.gaps);
        let mut writer = store.open_writer(&log).await.unwrap();
        match stream {
            LogStream::Normalized => {
                assert_eq!(snapshot.line_watermark, 40);
                assert_eq!(writer.line_watermark(), 40);
                // Continue past the gapped range in a fresh segment: the
                // attached segment ends at the committed boundary and the
                // gap must not be bridged inside one segment's frames.
                writer.seal(LogStream::Normalized).await.unwrap();
                writer.append_line(41, "forty-one").await.unwrap();
                writer.flush().await.unwrap();
                assert_eq!(
                    store
                        .read_committed(&log, LogStream::Normalized)
                        .await
                        .unwrap(),
                    b"oneforty-one".to_vec()
                );
            }
            LogStream::Raw => {
                assert_eq!(snapshot.raw_watermark, 40);
                assert_eq!(writer.raw_watermark(), 40);
                writer.seal(LogStream::Raw).await.unwrap();
                writer.append_raw(b"-tail").await.unwrap();
                writer.flush().await.unwrap();
                assert_eq!(
                    store.read_committed(&log, LogStream::Raw).await.unwrap(),
                    b"one-tail".to_vec()
                );
            }
        }
        writer.close().await.unwrap();
    }
}

/// A failure after the batch was extracted but before any frame was
/// written (segment creation refused on an occupied name): the stream
/// latches recovery-required, so a retry without recovery refuses and
/// the accepted range can never be silently skipped.
#[tokio::test]
async fn pre_frame_creation_failure_latches_instead_of_losing_the_batch() {
    let root = TempRoot::new("pre-frame");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();
    // The raw stream stays clean throughout (independent latch).
    writer.append_raw(b"raw").await.unwrap();
    writer.flush().await.unwrap();
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let next = snapshot
        .segments
        .iter()
        .map(|row| row.segment_id)
        .max()
        .unwrap()
        + 1;
    // Occupy the name the next creation would mint: creation must refuse.
    std::fs::write(root.join(format!("seg-{next:06}.log")), b"foreign").unwrap();
    // Sealing forces the next flush to create that fresh segment.
    writer.seal(LogStream::Normalized).await.unwrap();
    writer.append_line(2, "two").await.unwrap();
    match writer.flush().await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("already exists"), "{detail}");
        }
        other => panic!(
            "expected RecoveryRequired, got {:?}",
            other.map(|o| o.normalized)
        ),
    }
    // Retry without recovery refuses; the raw stream keeps working.
    assert!(matches!(
        writer.append_line(2, "two").await,
        Err(StorageError::RecoveryRequired { .. })
    ));
    writer.append_raw(b"-more").await.unwrap();
    // `flush` drains both streams; the poisoned normalized one refuses
    // (expected) while the raw stream still commits.
    let _ = writer.flush().await;
    // No silent skip: line 2 was never committed, so the durable
    // watermark still ends at 1.
    assert_eq!(
        store.recovery_snapshot(&log).await.unwrap().line_watermark,
        1
    );

    // Recovery tombstones the crashed creation and quarantines the
    // foreign file whole (never adopts it), then line 2 is re-appendable.
    writer.close().await.unwrap_err();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::ZeroByteRowTombstoned { segment_id: next }],
        "{:?}",
        report.actions
    );
    assert!(root.join(format!("quarantine-seg-{next:06}.log")).is_file());
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(2, "two").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-more".to_vec()
    );
    writer.close().await.unwrap();
}

/// The raw-stream half of the pre-frame regression: the creation of a
/// fresh raw segment onto an occupied name latches the raw stream (a
/// retry without recovery refuses; the accepted bytes are never silently
/// skipped) while the normalized stream of the same writer keeps
/// committing, and recovery then permits the re-append.
#[tokio::test]
async fn occupied_name_refusal_latches_the_raw_stream_too() {
    let root = TempRoot::new("pre-frame-raw");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.append_raw(b"raw").await.unwrap();
    writer.flush().await.unwrap();
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let next = snapshot
        .segments
        .iter()
        .map(|row| row.segment_id)
        .max()
        .unwrap()
        + 1;
    std::fs::write(root.join(format!("seg-{next:06}.log")), b"foreign").unwrap();
    writer.seal(LogStream::Raw).await.unwrap();
    writer.append_raw(b"-more").await.unwrap();
    match writer.flush().await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("already exists"), "{detail}");
        }
        other => panic!("expected RecoveryRequired, got {:?}", other.map(|o| o.raw)),
    }
    // Retry without recovery refuses; the normalized stream keeps working.
    assert!(matches!(
        writer.append_raw(b"-more").await,
        Err(StorageError::RecoveryRequired { .. })
    ));
    writer.append_line(2, "two").await.unwrap();
    let _ = writer.flush().await;
    // No silent skip: the raw bytes were never committed, so the durable
    // raw watermark still ends at 3.
    assert_eq!(
        store.recovery_snapshot(&log).await.unwrap().raw_watermark,
        3
    );

    writer.close().await.unwrap_err();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::ZeroByteRowTombstoned { segment_id: next }],
        "{:?}",
        report.actions
    );
    assert!(root.join(format!("quarantine-seg-{next:06}.log")).is_file());
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_raw(b"-more").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-more".to_vec()
    );
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    writer.close().await.unwrap();
}

/// One injectable pre-frame failure at `site` on `stream`, after the
/// batch was extracted: the flush fails there, the affected stream
/// latches recovery-required (a retry without recovery refuses — the
/// accepted range can never be silently skipped), the other stream of
/// the same writer stays usable, the durable watermark does not move,
/// and recovery converges so the exact range is re-appendable.
async fn pre_frame_failure_latches_without_silent_skip(stream: LogStream, site: FaultSite) {
    let root = TempRoot::new("pre-frame-matrix");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.set_rotation_threshold(2048).await;
    // Seed both streams with one committed ~1.4 KiB batch each, so the
    // faulted batch can also trigger the rotation seal on a live segment.
    let seed_line = "l-".to_owned() + &"x".repeat(1400);
    let seed_raw = vec![b'r'; 1400];
    writer.append_line(1, &seed_line).await.unwrap();
    writer.append_raw(&seed_raw).await.unwrap();
    writer.flush().await.unwrap();
    if site == FaultSite::ReclaimUnlink {
        // Grow to the metadata budget so the faulted creation reclaims a
        // sealed segment inside its own transaction.
        let mut line = 1u64;
        loop {
            line += 1;
            writer.append_line(line, &seed_line).await.unwrap();
            writer.flush().await.unwrap();
            if store.recovery_snapshot(&log).await.unwrap().segments.len()
                == MAX_SEGMENT_METADATA_ROWS
            {
                break;
            }
        }
    }
    let line_mark = writer.line_watermark();
    let raw_mark = writer.raw_watermark();

    // Steer the faulted flush through its pre-frame operation: the
    // rotation seal for `SealSync` (a live segment must be sealed first),
    // a fresh-segment creation for every other site.
    if site != FaultSite::SealSync {
        writer.seal(stream).await.unwrap();
    }
    writer.set_fault_once(site).await;
    let flush = match stream {
        LogStream::Normalized => {
            writer.append_line(line_mark + 1, &seed_line).await.unwrap();
            writer.flush().await
        }
        LogStream::Raw => {
            writer.append_raw(&seed_raw).await.unwrap();
            writer.flush().await
        }
    };
    match flush {
        Err(StorageError::Io { .. }) => {}
        other => panic!(
            "{stream:?}/{site:?}: expected the injected Io failure, got {:?}",
            other.map(|o| (o.normalized, o.raw))
        ),
    }
    // The affected stream refuses retries without recovery.
    match stream {
        LogStream::Normalized => assert!(
            matches!(
                writer.append_line(line_mark + 1, "retry").await,
                Err(StorageError::RecoveryRequired { .. })
            ),
            "{stream:?}/{site:?}"
        ),
        LogStream::Raw => assert!(
            matches!(
                writer.append_raw(b"retry").await,
                Err(StorageError::RecoveryRequired { .. })
            ),
            "{stream:?}/{site:?}"
        ),
    }
    // The other stream of the same writer stays usable, and its batch
    // commits through the shared flush.
    match stream {
        LogStream::Normalized => {
            writer.append_raw(b"-more").await.unwrap();
        }
        LogStream::Raw => {
            writer.append_line(line_mark + 1, "more").await.unwrap();
        }
    }
    let _ = writer.flush().await;
    // No silent skip: the failed batch was never committed, so the
    // affected stream's durable watermark never moved past its seed
    // (read directly from the database: the faulted creation can leave a
    // zero-committed row without a file, which the file-reading surfaces
    // rightly refuse until recovery ran).
    let pool = raw_pool(&root).await;
    let (line_watermark, raw_watermark): (i64, i64) =
        sqlx::query_as("SELECT line_watermark, raw_watermark FROM terminal")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    match stream {
        LogStream::Normalized => assert_eq!(
            u64::try_from(line_watermark).unwrap(),
            line_mark,
            "{stream:?}/{site:?}"
        ),
        LogStream::Raw => assert_eq!(
            u64::try_from(raw_watermark).unwrap(),
            raw_mark,
            "{stream:?}/{site:?}"
        ),
    }
    // The latched stream makes close's final drain refuse (expected; the
    // joined driver still releases the lease).
    writer.close().await.unwrap_err();

    // Recovery converges and the exact range is re-appendable.
    let _report = store.recover(&log).await.unwrap();
    // The durable bytes recovery left: the other stream's committed
    // batches survive untouched (a reclaim inside the faulted creation is
    // already reflected in its retained range), and the affected stream
    // is back at its committed seed.
    let expected_norm = store
        .read_committed(&log, LogStream::Normalized)
        .await
        .unwrap();
    let expected_raw = store.read_committed(&log, LogStream::Raw).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    match stream {
        LogStream::Normalized => {
            assert_eq!(writer.line_watermark(), line_mark, "{stream:?}/{site:?}");
            writer
                .append_line(line_mark + 1, "re-appended")
                .await
                .unwrap();
        }
        LogStream::Raw => {
            assert_eq!(writer.raw_watermark(), raw_mark, "{stream:?}/{site:?}");
            writer.append_raw(b"re-appended").await.unwrap();
        }
    }
    writer.flush().await.unwrap();
    writer.close().await.unwrap();
    match stream {
        LogStream::Normalized => {
            let mut expected = expected_norm;
            expected.extend_from_slice(b"re-appended");
            assert_eq!(
                store
                    .read_committed(&log, LogStream::Normalized)
                    .await
                    .unwrap(),
                expected,
                "{stream:?}/{site:?}"
            );
            assert_eq!(
                store.read_committed(&log, LogStream::Raw).await.unwrap(),
                expected_raw,
                "{stream:?}/{site:?}"
            );
        }
        LogStream::Raw => {
            let mut expected = expected_raw;
            expected.extend_from_slice(b"re-appended");
            assert_eq!(
                store.read_committed(&log, LogStream::Raw).await.unwrap(),
                expected,
                "{stream:?}/{site:?}"
            );
            assert_eq!(
                store
                    .read_committed(&log, LogStream::Normalized)
                    .await
                    .unwrap(),
                expected_norm,
                "{stream:?}/{site:?}"
            );
        }
    }
    let rerun = store.recover(&log).await.unwrap();
    assert!(
        rerun.actions.is_empty(),
        "{stream:?}/{site:?}: {:?}",
        rerun.actions
    );
}

/// Every injectable pre-frame failure after batch extraction — the
/// rotation seal's file sync, the reclaimed-segment unlink, the fresh
/// header write, the header sync, and the creation-path directory sync —
/// on both streams.
#[tokio::test]
async fn pre_frame_injection_sites_latch_without_silent_skip_on_both_streams() {
    for stream in [LogStream::Normalized, LogStream::Raw] {
        for site in [
            FaultSite::SealSync,
            FaultSite::ReclaimUnlink,
            FaultSite::HeaderWrite,
            FaultSite::HeaderSync,
            FaultSite::DirSync,
        ] {
            pre_frame_failure_latches_without_silent_skip(stream, site).await;
        }
    }
}

/// The sub-64KiB explicit flush surfaces a dropped batch as its outcome.
#[tokio::test]
async fn explicit_flush_reports_a_dropped_batch() {
    let root = TempRoot::new("flush-drop");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    saturate_budget(&store, &log).await;
    make_reclaim_unsafe(&root).await;
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.set_rotation_threshold(2048).await;
    let watermark = writer.line_watermark();
    let committed_prefix = store
        .read_committed(&log, LogStream::Normalized)
        .await
        .unwrap();
    // A small batch: sealing forces a fresh segment, so only the explicit
    // flush reaches the unsafe creation.
    writer.seal(LogStream::Normalized).await.unwrap();
    writer
        .append_line(watermark + 1, "small-batch")
        .await
        .unwrap();
    let outcomes = writer.flush().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Dropped);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Nothing);
    // Durable effect of the drop: explicit gap, consumed numbers, latch.
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded && snapshot.refuse_new_start);
    assert_eq!(snapshot.line_watermark, watermark + 1);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].start, watermark + 1);
    assert_eq!(snapshot.gaps[0].end, watermark + 2);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        committed_prefix
    );
    writer.close().await.unwrap();
}

/// The 50 ms timer path: the driver-initiated drop is recorded durably
/// and observable through `last_flush_outcome` while the producer keeps
/// draining (both streams drop independently).
#[tokio::test]
async fn timer_flush_drop_is_observable_while_draining_continues() {
    let root = TempRoot::new("timer-drop");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    saturate_budget(&store, &log).await;
    make_reclaim_unsafe(&root).await;
    let mut writer = store.open_writer(&log).await.unwrap();
    let line_mark = writer.line_watermark();
    let raw_mark = writer.raw_watermark();
    // Small appends into streams with no active segment (sealed above):
    // the deadline driver owns both creation-failing flushes.
    writer.seal(LogStream::Normalized).await.unwrap();
    writer.seal(LogStream::Raw).await.unwrap();
    writer
        .append_line(line_mark + 1, "timer-one")
        .await
        .unwrap();
    writer.append_raw(b"timer-raw").await.unwrap();
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Nothing,
        "no batch-bearing flush ran yet"
    );
    tokio::time::sleep(FLUSH_MAX_DELAY * 3).await;
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Dropped
    );
    assert_eq!(
        writer.last_flush_outcome(LogStream::Raw),
        StreamFlushOutcome::Dropped
    );
    // The producer keeps draining: further appends are still accepted and
    // each records its own widening drop.
    writer
        .append_line(line_mark + 2, "timer-two")
        .await
        .unwrap();
    tokio::time::sleep(FLUSH_MAX_DELAY * 3).await;
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Dropped
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(snapshot.line_watermark, line_mark + 2);
    assert_eq!(snapshot.raw_watermark, raw_mark + b"timer-raw".len() as u64);
    assert_eq!(snapshot.gaps.len(), 2, "{:?}", snapshot.gaps);
    assert!(snapshot.degraded && snapshot.refuse_new_start);
    // `close` surfaces the driver's final drop too: its drain finds
    // nothing pending, so it reports each stream's most recent
    // batch-bearing outcome — both dropped, never a false `Nothing`.
    let outcomes = writer.close().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Dropped);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Dropped);
}

/// The timer-driven commit through `close`: the 50 ms driver committed
/// the trailing batches of both streams, so `close`'s final drain finds
/// nothing pending — and must report the recorded committed outcomes
/// instead of a false `Nothing` (the owner notification of the final
/// durable outcome).
#[tokio::test]
async fn close_reports_the_timer_driven_commit_instead_of_nothing() {
    let root = TempRoot::new("close-timer-commit");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.append_raw(b"raw-one").await.unwrap();
    tokio::time::sleep(FLUSH_MAX_DELAY * 3).await;
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Committed
    );
    assert_eq!(
        writer.last_flush_outcome(LogStream::Raw),
        StreamFlushOutcome::Committed
    );
    let outcomes = writer.close().await.unwrap();
    assert_eq!(
        outcomes.normalized,
        StreamFlushOutcome::Committed,
        "close must report the driver's commit, not Nothing"
    );
    assert_eq!(outcomes.raw, StreamFlushOutcome::Committed);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"one".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-one".to_vec()
    );
}

/// Graceful `close` while the final flush is parked at a write/sync/
/// commit boundary: the lease stays held (no recovery, no reopen) until
/// the driver-driven flush completes, `close` joins it and returns the
/// committed outcome, and a new writer attaches straight afterwards.
#[tokio::test]
async fn close_joins_a_parked_flush_and_holds_the_lease_until_it_exits() {
    for point in [
        ParkPoint::AfterFrameWrite,
        ParkPoint::AfterFileSync,
        ParkPoint::AfterCommit,
    ] {
        let root = TempRoot::new("close-parked");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "first").await.unwrap();
        writer.flush().await.unwrap();

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                point,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        let closing = tokio::spawn(async move {
            writer.append_line(2, "second").await.unwrap();
            writer.close().await
        });
        reached_rx.await.unwrap();
        // While the shutdown flush is parked inside `close`, the lease is
        // still held: neither a new writer nor recovery may attach.
        assert!(
            matches!(
                store.open_writer(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: reopen must wait for the driver to exit"
        );
        assert!(matches!(
            store.recover(&log).await,
            Err(StorageError::WriterAlreadyActive { .. })
        ));
        release_tx.send(()).unwrap();
        let outcomes = closing.await.unwrap().unwrap();
        assert_eq!(
            outcomes.normalized,
            StreamFlushOutcome::Committed,
            "{point:?}"
        );

        // The driver has exited and the lease is free: data is durable and
        // a fresh writer continues the numbering immediately.
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"firstsecond".to_vec()
        );
        let mut next = store.open_writer(&log).await.unwrap();
        assert_eq!(next.line_watermark(), 2);
        next.append_line(3, "third").await.unwrap();
        next.flush().await.unwrap();
        next.close().await.unwrap();
    }
}

/// Dropping without `close` stays abort-only, and the lease it leaves
/// behind is released by the kernel/teardown soon enough that a reopen
/// (through the bounded lease wait) succeeds and recovery converges.
#[tokio::test]
async fn dropped_writer_releases_the_lease_promptly_for_reopen_and_recovery() {
    let root = TempRoot::new("drop-lease");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(2, "two").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    writer.close().await.unwrap();
    assert!(store.recover(&log).await.unwrap().is_empty());
}

/// Dropping without `close` never aborts the flush driver: at every
/// park point of the commit sequence, a writer dropped while its
/// deadline driver is mid-operation keeps the exclusive lease (no
/// reopen, no recovery) until the detached driver finishes the in-flight
/// operation and exits — and the operation's batch is durable, so
/// recovery afterwards converges over a clean log.
#[tokio::test]
async fn drop_detaches_the_driver_and_holds_the_lease_through_in_flight_io() {
    for point in [
        ParkPoint::AfterFrameWrite,
        ParkPoint::AfterFileSync,
        ParkPoint::AfterCommit,
    ] {
        let root = TempRoot::new("drop-parked");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "first").await.unwrap();
        writer.flush().await.unwrap();

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                point,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        // A small batch stays buffered; the 50 ms deadline driver owns
        // its flush and parks mid-commit-sequence.
        writer.append_line(2, "second").await.unwrap();
        reached_rx.await.unwrap();

        // Drop — not close: the driver must not be cancelled out of its
        // in-flight operation, so the lease stays held while it is parked.
        drop(writer);
        assert!(
            matches!(
                store.open_writer(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: reopen must wait for the detached driver to exit"
        );
        assert!(
            matches!(
                store.recover(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: recovery must wait for the detached driver to exit"
        );

        // Releasing the park lets the detached driver finish the operation
        // (write/sync/commit/publish as far as the point allows) and exit;
        // the lease releases exactly then and the in-flight batch is
        // durable — the bounded lease wait covers the driver's teardown.
        release_tx.send(()).unwrap();
        let next = store.open_writer(&log).await.unwrap();
        assert_eq!(next.line_watermark(), 2, "{point:?}");
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"firstsecond".to_vec(),
            "{point:?}: the in-flight flush completed before the lease released"
        );
        next.close().await.unwrap();
        // Nothing was left to repair: the completed flush left no tail.
        assert!(
            store.recover(&log).await.unwrap().is_empty(),
            "{point:?}: recovery must converge over the completed flush"
        );
    }
}

/// Handoff race: a waiter queues on the exclusive lease while the owner
/// is live; the owner then commits both streams and closes inside the
/// waiter's bounded acquisition window. The waiter must attach at the
/// owner's *final* durable watermarks — its terminal row is read under
/// the lease — so numbering continues at exactly the handed-off
/// coordinates with no reuse and no commit conflict.
#[tokio::test]
async fn waiter_attaches_at_the_owners_final_durable_watermarks_after_handoff() {
    let root = TempRoot::new("handoff");
    let store = std::sync::Arc::new(LogStore::open(&root).await.unwrap());
    let log = log_identity();
    let mut owner = store.open_writer(&log).await.unwrap();
    owner.append_line(1, "one").await.unwrap();
    owner.append_raw(b"raw-one").await.unwrap();
    owner.flush().await.unwrap();

    // The waiter starts while the owner is attached: it can only queue on
    // the lease (bounded retry) until the owner releases it.
    let waiter_store = std::sync::Arc::clone(&store);
    let waiter_log = log.clone();
    let waiter = tokio::spawn(async move { waiter_store.open_writer(&waiter_log).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The owner commits further batches on both streams and closes inside
    // the 1 s acquisition window.
    owner.append_line(2, "two").await.unwrap();
    owner.append_raw(b"raw-two").await.unwrap();
    owner.flush().await.unwrap();
    owner.close().await.unwrap();

    let mut next = waiter.await.unwrap().unwrap();
    // The waiter attached at the owner's final durable state, not the
    // snapshot from before it queued on the lease.
    assert_eq!(next.line_watermark(), 2);
    assert_eq!(next.raw_watermark(), b"raw-oneraw-two".len() as u64);
    // Numbering continues exactly at the handed-off watermarks: no
    // coordinate is reused and no compare-and-set refuses.
    next.append_line(3, "three").await.unwrap();
    next.append_raw(b"raw-three").await.unwrap();
    next.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwothree".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-oneraw-tworaw-three".to_vec()
    );
    next.close().await.unwrap();
}

/// The other half of the stale-snapshot hazard: a `refuse_new_start`
/// latch set while a waiter was queued on the lease must refuse that
/// waiter — the latch is read under the lease, never from the pre-wait
/// snapshot.
#[tokio::test]
async fn waiter_refuses_a_latch_latched_while_it_waited_for_the_lease() {
    let root = TempRoot::new("handoff-latch");
    let store = std::sync::Arc::new(LogStore::open(&root).await.unwrap());
    let log = log_identity();
    let mut owner = store.open_writer(&log).await.unwrap();
    owner.append_line(1, "one").await.unwrap();
    owner.flush().await.unwrap();

    let waiter_store = std::sync::Arc::clone(&store);
    let waiter_log = log.clone();
    let waiter = tokio::spawn(async move { waiter_store.open_writer(&waiter_log).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // While the waiter queues, the latch is set (the durable effect of a
    // drain overflow or an unsafe retention reclaim) and the owner
    // closes: the waiter must observe the latch, not bypass it.
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE terminal SET refuse_new_start = 1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    owner.close().await.unwrap();

    assert!(
        matches!(
            waiter.await.unwrap(),
            Err(StorageError::RefuseNewStart { .. })
        ),
        "a latch set while the waiter waited must refuse the attach"
    );
}

/// The sub-64KiB size-bound path keeps reporting `Committed` (the append
/// that reaches the bound flushes itself), and `Nothing` never overwrites
/// a real outcome.
#[tokio::test]
async fn outcome_bookkeeping_distinguishes_committed_from_nothing() {
    let root = TempRoot::new("outcome-clean");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    let outcomes = writer.flush().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Nothing);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Nothing);
    writer.append_line(1, "one").await.unwrap();
    let outcomes = writer.flush().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Committed);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Nothing);
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Committed
    );
    let outcomes = writer.flush().await.unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Nothing);
    assert_eq!(
        writer.last_flush_outcome(LogStream::Normalized),
        StreamFlushOutcome::Committed,
        "a Nothing flush must not erase the last real outcome"
    );
    // The size-bound append still reports Committed on the append itself.
    let text = "y".repeat(FLUSH_MAX_BYTES);
    let appended = writer.append_line(2, &text).await.unwrap();
    assert_eq!(appended.outcome, qingluan_storage::AppendOutcome::Committed);
    writer.close().await.unwrap();
}

/// Poll until the exclusive lease is free and a writer can attach again:
/// the detached command (and flush-driver) tasks finish their in-flight
/// persistence and release it. Bounded, so a lost lease fails the test
/// instead of hanging it.
async fn reopen_when_free(store: &LogStore, log: &LogIdentity) -> LogWriter {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match store.open_writer(log).await {
            Ok(writer) => return writer,
            Err(StorageError::WriterAlreadyActive { .. }) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the detached writer tasks never released the lease"
                );
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            Err(other) => panic!("unexpected reopen result: {other:?}"),
        }
    }
}

/// Task-abort of a foreground append at the filesystem and SQLite
/// boundaries of its size-bound flush: the acceptance and the flush run
/// inside a spawned command task, so aborting the caller detaches — never
/// cancels — the persistence work. While the command is parked
/// mid-operation the exclusive lease is still held (neither a reopen nor
/// the recovery pass may interleave with the in-flight fs/SQLite work);
/// once released, the command completes, the lease frees, and the log
/// converges over the durable batch with a clean recovery pass. Both
/// streams, every boundary.
#[tokio::test]
async fn aborted_foreground_append_holds_the_lease_then_converges() {
    for stream in [LogStream::Normalized, LogStream::Raw] {
        for point in [
            ParkPoint::AfterFrameWrite,
            ParkPoint::AfterFileSync,
            ParkPoint::AfterCommit,
        ] {
            let root = TempRoot::new("abort-append");
            let store = LogStore::open(&root).await.unwrap();
            let log = log_identity();
            let mut writer = store.open_writer(&log).await.unwrap();
            // Seed one committed batch so the aborted batch appends behind
            // a live segment on a clean baseline.
            match stream {
                LogStream::Normalized => {
                    writer.append_line(1, "first").await.unwrap();
                }
                LogStream::Raw => {
                    writer.append_raw(b"first").await.unwrap();
                }
            }
            writer.flush().await.unwrap();

            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            writer
                .set_park_once(
                    point,
                    Box::pin(async move {
                        let _ = reached_tx.send(());
                        let _ = release_rx.await;
                    }),
                )
                .await;
            // The append that reaches the 64 KiB batch bound flushes itself
            // synchronously inside its command task.
            let appending = match stream {
                LogStream::Normalized => {
                    let text = "x".repeat(FLUSH_MAX_BYTES);
                    tokio::spawn(async move { writer.append_line(2, &text).await.map(|_| ()) })
                }
                LogStream::Raw => {
                    let bytes = vec![b'r'; FLUSH_MAX_BYTES];
                    tokio::spawn(async move { writer.append_raw(&bytes).await.map(|_| ()) })
                }
            };
            reached_rx.await.unwrap();
            // Abort the caller: the detached command keeps the lease and
            // the in-flight operation.
            appending.abort();
            assert!(appending.await.unwrap_err().is_cancelled());

            // Exclusion: the aborted append's persistence still owns the
            // lease, so neither a reopen nor the recovery pass may run
            // while it is parked mid-operation.
            assert!(
                matches!(
                    store.open_writer(&log).await,
                    Err(StorageError::WriterAlreadyActive { .. })
                ),
                "{stream:?}/{point:?}: reopen must wait for the detached command"
            );
            assert!(
                matches!(
                    store.recover(&log).await,
                    Err(StorageError::WriterAlreadyActive { .. })
                ),
                "{stream:?}/{point:?}: recovery must wait for the detached command"
            );

            // Convergence: release the park, let the command finish, and
            // the lease frees at exactly the durable state it produced.
            release_tx.send(()).unwrap();
            let next = reopen_when_free(&store, &log).await;
            let expected: Vec<u8> = match stream {
                LogStream::Normalized => {
                    assert_eq!(next.line_watermark(), 2, "{stream:?}/{point:?}");
                    let mut expected = b"first".to_vec();
                    expected.extend(std::iter::repeat_n(b'x', FLUSH_MAX_BYTES));
                    expected
                }
                LogStream::Raw => {
                    assert_eq!(
                        next.raw_watermark(),
                        5 + FLUSH_MAX_BYTES as u64,
                        "{stream:?}/{point:?}"
                    );
                    let mut expected = b"first".to_vec();
                    expected.extend(std::iter::repeat_n(b'r', FLUSH_MAX_BYTES));
                    expected
                }
            };
            next.close().await.unwrap();
            assert_eq!(
                store.read_committed(&log, stream).await.unwrap(),
                expected,
                "{stream:?}/{point:?}: the aborted append's batch must be durable"
            );
            let report = store.recover(&log).await.unwrap();
            assert!(
                report.is_empty(),
                "{stream:?}/{point:?}: recovery must converge over the completed flush: {:?}",
                report.actions
            );
        }
    }
}

/// Task-abort of an explicit [`qingluan_storage::LogWriter::flush`] (at
/// the SQLite boundary) and of a [`qingluan_storage::LogWriter::seal`]
/// (at the filesystem boundary): the detached command keeps the lease
/// until the flush — and, for the seal, the segment's file sync and row
/// seal — truly completes, then the log converges.
#[tokio::test]
async fn aborted_flush_and_seal_hold_the_lease_then_converges() {
    // flush: park after the visibility transaction, before the publish.
    {
        let root = TempRoot::new("abort-flush");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "one").await.unwrap();
        writer.flush().await.unwrap();
        writer.append_line(2, "two").await.unwrap();

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                ParkPoint::AfterCommit,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        let flushing = tokio::spawn(async move { writer.flush().await });
        reached_rx.await.unwrap();
        flushing.abort();
        assert!(flushing.await.unwrap_err().is_cancelled());
        assert!(matches!(
            store.open_writer(&log).await,
            Err(StorageError::WriterAlreadyActive { .. })
        ));
        release_tx.send(()).unwrap();
        let next = reopen_when_free(&store, &log).await;
        assert_eq!(next.line_watermark(), 2);
        next.close().await.unwrap();
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"onetwo".to_vec()
        );
        assert!(store.recover(&log).await.unwrap().is_empty());
    }
    // seal: park after the pending batch's frames are written, before
    // they are synced — the seal's flush is still in flight.
    {
        let root = TempRoot::new("abort-seal");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "one").await.unwrap();
        writer.flush().await.unwrap();
        writer.append_line(2, "two").await.unwrap();

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                ParkPoint::AfterFrameWrite,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        let sealing = tokio::spawn(async move { writer.seal(LogStream::Normalized).await });
        reached_rx.await.unwrap();
        sealing.abort();
        assert!(sealing.await.unwrap_err().is_cancelled());
        assert!(matches!(
            store.open_writer(&log).await,
            Err(StorageError::WriterAlreadyActive { .. })
        ));
        release_tx.send(()).unwrap();
        let next = reopen_when_free(&store, &log).await;
        assert_eq!(next.line_watermark(), 2);
        next.close().await.unwrap();
        // The seal completed: the seeded segment's row is sealed and the
        // sealed-away batch reads back.
        let snapshot = store.recovery_snapshot(&log).await.unwrap();
        assert!(
            snapshot
                .segments
                .iter()
                .any(|row| row.kind == LogStream::Normalized && row.state == "sealed")
        );
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"onetwo".to_vec()
        );
        assert!(store.recover(&log).await.unwrap().is_empty());
    }
}

/// A cancelled `close`: aborting the shutdown future while its trailing
/// batch's flush is parked mid-operation detaches — never cancels — the
/// persistence work (the final drain command, or the joined flush
/// driver's in-flight deadline flush; whichever reached the park, the
/// invariant is the same). The lease stays held until the flush truly
/// completes; then the log converges with the trailing batch durable
/// and a clean recovery pass, at every boundary.
#[tokio::test]
async fn aborted_close_final_drain_holds_the_lease_then_converges() {
    for point in [
        ParkPoint::AfterFrameWrite,
        ParkPoint::AfterFileSync,
        ParkPoint::AfterCommit,
    ] {
        let root = TempRoot::new("abort-close");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "first").await.unwrap();
        writer.flush().await.unwrap();

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                point,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        let closing = tokio::spawn(async move {
            writer.append_line(2, "second").await.unwrap();
            writer.close().await
        });
        reached_rx.await.unwrap();
        closing.abort();
        assert!(closing.await.unwrap_err().is_cancelled());
        // The shutdown flush still owns the lease while parked.
        assert!(
            matches!(
                store.open_writer(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: reopen must wait for the detached shutdown flush"
        );

        release_tx.send(()).unwrap();
        let next = reopen_when_free(&store, &log).await;
        assert_eq!(next.line_watermark(), 2, "{point:?}");
        next.close().await.unwrap();
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"firstsecond".to_vec(),
            "{point:?}: the cancelled close's trailing batch must be durable"
        );
        assert!(
            store.recover(&log).await.unwrap().is_empty(),
            "{point:?}: recovery must converge over the completed shutdown flush"
        );
    }
}

/// The S2 oracle's close-ownership blocker: `close` used to await the
/// flush driver *before* creating the owned final-drain task, so a
/// caller aborted during that join dropped the whole shutdown sequence —
/// the pending batches were never drained and the lease could release
/// with data still buffered. The regression aborts `close` while the
/// driver is parked mid-flush of one stream (its 50 ms deadline flush)
/// and the other stream holds later pending data: neither a reopen nor
/// recovery may attach until the owned shutdown completes (driver joined,
/// commands quiesced, both streams drained), and both streams then
/// converge — the parked stream through the driver's own flush, the
/// later stream through the final drain.
#[tokio::test]
async fn aborted_close_while_driver_parked_holds_the_lease_until_shutdown_completes() {
    for point in [
        ParkPoint::AfterFrameWrite,
        ParkPoint::AfterFileSync,
        ParkPoint::AfterCommit,
    ] {
        let root = TempRoot::new("abort-close-driver");
        let store = LogStore::open(&root).await.unwrap();
        let log = log_identity();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "first").await.unwrap();
        writer.flush().await.unwrap();

        // The normalized batch's deadline opens first; a later raw append
        // (a strictly later deadline) must stay pending behind the parked
        // driver, which holds the state lock inside its flush.
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        writer
            .set_park_once(
                point,
                Box::pin(async move {
                    let _ = reached_tx.send(());
                    let _ = release_rx.await;
                }),
            )
            .await;
        writer.append_line(2, "second").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        writer.append_raw(b"raw-pending").await.unwrap();
        // Wait for the driver to be parked inside its deadline flush of
        // the normalized stream *before* starting `close`: the abort must
        // provably land on the close's driver join, never on a drain that
        // consumed the park instead.
        reached_rx.await.unwrap();

        // `close` is entered (its first poll runs synchronously through
        // spawning the owned shutdown task / reaching the driver join)
        // and then aborted while the driver it must join is still parked
        // mid-flush: the whole shutdown sequence is owned by a spawned
        // task created before the method's first await, so the abort
        // detaches — never drops — it.
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let closing = tokio::spawn(async move {
            let _ = entered_tx.send(());
            writer.close().await
        });
        entered_rx.await.unwrap();
        closing.abort();
        assert!(closing.await.unwrap_err().is_cancelled());

        // Exclusion while the owned shutdown is still in flight: the
        // detached sequence holds the shared state (and with it the
        // lease) exactly until driver join, quiesce, and final drain are
        // all truly complete.
        assert!(
            matches!(
                store.open_writer(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: reopen must wait for the owned shutdown to finish"
        );
        assert!(
            matches!(
                store.recover(&log).await,
                Err(StorageError::WriterAlreadyActive { .. })
            ),
            "{point:?}: recovery must wait for the owned shutdown to finish"
        );

        // Completing the parked driver flush lets the owned shutdown run
        // to completion: driver joined, commands quiesced, both streams
        // drained — the parked stream by the driver, the later stream by
        // the final drain.
        release_tx.send(()).unwrap();
        let next = reopen_when_free(&store, &log).await;
        assert_eq!(next.line_watermark(), 2, "{point:?}");
        assert_eq!(
            next.raw_watermark(),
            b"raw-pending".len() as u64,
            "{point:?}"
        );
        next.close().await.unwrap();
        assert_eq!(
            store
                .read_committed(&log, LogStream::Normalized)
                .await
                .unwrap(),
            b"firstsecond".to_vec(),
            "{point:?}: the parked driver flush must be durable"
        );
        assert_eq!(
            store.read_committed(&log, LogStream::Raw).await.unwrap(),
            b"raw-pending".to_vec(),
            "{point:?}: the cancelled close's final drain must be durable"
        );
        assert!(
            store.recover(&log).await.unwrap().is_empty(),
            "{point:?}: recovery must converge over the completed shutdown"
        );
    }
}

/// The quiesce half of the owned shutdown: a foreground append whose
/// caller is cancelled mid-operation leaves a detached command parked at
/// a durability boundary. `close` — even started while that command is
/// parked — must not return before the command's persistence work truly
/// landed (no commit may race or outlive the close), and the lease stays
/// held for exactly that long.
#[tokio::test]
async fn close_quiesces_a_detached_foreground_command_before_returning() {
    let root = TempRoot::new("close-quiesce");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "first").await.unwrap();
    writer.flush().await.unwrap();

    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    writer
        .set_park_once(
            ParkPoint::AfterCommit,
            Box::pin(async move {
                let _ = reached_tx.send(());
                let _ = release_rx.await;
            }),
        )
        .await;
    // The append that reaches the 64 KiB batch bound flushes itself
    // inside its command task; cancelling only the caller detaches it.
    let text = "x".repeat(FLUSH_MAX_BYTES);
    let mut appending = Box::pin(writer.append_line(2, &text));
    tokio::select! {
        result = &mut appending => {
            panic!("the append's flush must be parked, not finished: {result:?}");
        }
        _ = reached_rx => {}
    }
    drop(appending);
    // The detached command still owns the lease while parked: no reopen.
    assert!(matches!(
        store.open_writer(&log).await,
        Err(StorageError::WriterAlreadyActive { .. })
    ));

    // `close` starts while the detached command is parked; it may not
    // return ahead of the command's in-flight persistence (its final
    // drain would otherwise report around, and its lease release would
    // outrun, work that is still committing).
    let closing = tokio::spawn(async move { writer.close().await });
    assert!(matches!(
        store.open_writer(&log).await,
        Err(StorageError::WriterAlreadyActive { .. })
    ));
    release_tx.send(()).unwrap();
    let outcomes = closing.await.unwrap().unwrap();
    assert_eq!(outcomes.normalized, StreamFlushOutcome::Committed);
    assert_eq!(outcomes.raw, StreamFlushOutcome::Nothing);

    // The quiesced command's batch is durable before the close returned,
    // and the lease is free: a fresh writer continues the numbering.
    let next = store.open_writer(&log).await.unwrap();
    assert_eq!(next.line_watermark(), 2);
    next.close().await.unwrap();
    let mut expected = b"first".to_vec();
    expected.extend(std::iter::repeat_n(b'x', FLUSH_MAX_BYTES));
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        expected,
        "the detached append's batch must be durable before close returned"
    );
    assert!(store.recover(&log).await.unwrap().is_empty());
}

/// The close-drain isolation blocker: the final drain used to propagate
/// the first stream's failure before attempting the second, so a
/// poisoned normalized stream stranded the raw stream's trailing batch.
/// With a poisoned normalized stream and pending raw bytes, `close` must
/// still commit the raw batch (matching `flush`'s both-attempted
/// isolation) and report the normalized failure deterministically.
#[tokio::test]
async fn close_drains_the_healthy_stream_and_reports_the_poisoned_stream() {
    let root = TempRoot::new("close-isolation");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.append_raw(b"raw-one").await.unwrap();
    writer.flush().await.unwrap();
    // Poison the normalized stream: its next segment creation must fail
    // on an occupied name (an unrecovered foreign file).
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let next = snapshot
        .segments
        .iter()
        .map(|row| row.segment_id)
        .max()
        .unwrap()
        + 1;
    std::fs::write(root.join(format!("seg-{next:06}.log")), b"foreign").unwrap();
    writer.seal(LogStream::Normalized).await.unwrap();
    // Pending data on both streams: the normalized batch can only fail,
    // the raw batch must become durable through the final drain.
    writer.append_line(2, "two").await.unwrap();
    writer.append_raw(b"raw-two").await.unwrap();

    match writer.close().await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("already exists"), "{detail}");
        }
        other => panic!("close must report the normalized failure, got {other:?}"),
    }
    // The healthy stream was still drained and became durable.
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-oneraw-two".to_vec(),
        "the raw stream's trailing batch must be durable despite the poisoned stream"
    );
    // Recovery converges: the failed creation's empty row is tombstoned
    // and the foreign file quarantined, so the exact range is
    // re-appendable. (The normalized read runs after the pass: the
    // tombstoned creation's row points at the occupied name until then.)
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::ZeroByteRowTombstoned { segment_id: next }],
        "{:?}",
        report.actions
    );
    assert!(root.join(format!("quarantine-seg-{next:06}.log")).is_file());
    // The poisoned stream's pending batch was never committed: its
    // durable stream still ends at the pre-poison committed prefix.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"one".to_vec()
    );
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 1);
    assert_eq!(writer.raw_watermark(), b"raw-oneraw-two".len() as u64);
    writer.append_line(2, "two").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    writer.close().await.unwrap();
}

/// The explicit-seal failure blocker (both fallible steps, both streams):
/// the seal used to remove the active segment from the stream state
/// before the file sync and row seal, so a failure left the writer with
/// no active segment while the row was still the stream's active row —
/// the next append then created a second active segment behind it. Every
/// explicit seal failure now restores the segment and latches the stream
/// recovery-required: appends refuse until recovery, exactly one active
/// row remains, and the log converges so the seal can be retried.
#[tokio::test]
async fn explicit_seal_failure_latches_the_stream_and_keeps_one_active_segment() {
    for stream in [LogStream::Normalized, LogStream::Raw] {
        for (site, expect_io) in [(FaultSite::SealSync, true), (FaultSite::SealDb, false)] {
            let root = TempRoot::new("seal-fail");
            let store = LogStore::open(&root).await.unwrap();
            let log = log_identity();
            let mut writer = store.open_writer(&log).await.unwrap();
            match stream {
                LogStream::Normalized => {
                    writer.append_line(1, "one").await.unwrap();
                }
                LogStream::Raw => {
                    writer.append_raw(b"raw-one").await.unwrap();
                }
            }
            writer.flush().await.unwrap();

            // The explicit seal fails at the injected step.
            writer.set_fault_once(site).await;
            match writer.seal(stream).await {
                Err(error) => {
                    if expect_io {
                        assert!(
                            matches!(error, StorageError::Io { .. }),
                            "{stream:?}/{site:?}: {error}"
                        );
                    } else {
                        assert!(
                            matches!(error, StorageError::Database(_)),
                            "{stream:?}/{site:?}: {error}"
                        );
                    }
                }
                Ok(()) => panic!("{stream:?}/{site:?}: the injected seal fault must fail"),
            }
            // The stream refuses every further use until recovery…
            let refuse = match stream {
                LogStream::Normalized => writer.append_line(2, "two").await.map(|_| ()),
                LogStream::Raw => writer.append_raw(b"raw-two").await.map(|_| ()),
            };
            assert!(
                matches!(refuse, Err(StorageError::RecoveryRequired { .. })),
                "{stream:?}/{site:?}: a failed seal must latch the stream"
            );
            // …while the other stream keeps committing.
            match stream {
                LogStream::Normalized => {
                    writer.append_raw(b"raw-continues").await.unwrap();
                }
                LogStream::Raw => {
                    writer.append_line(1, "norm-continues").await.unwrap();
                }
            }
            writer.flush().await.unwrap_err();
            // (the latched stream's refusal is the error; the healthy
            // stream still committed inside that same flush)

            // No stale extra active row: the stream's single row is still
            // its active segment (not sealed, not shadowed by a fresh one
            // created behind the failed seal).
            let snapshot = store.recovery_snapshot(&log).await.unwrap();
            let rows: Vec<_> = snapshot
                .segments
                .iter()
                .filter(|row| row.kind == stream)
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "{stream:?}/{site:?}: a failed seal must not leave a second segment row: {:?}",
                snapshot.segments
            );
            assert_eq!(rows[0].state, "active", "{stream:?}/{site:?}");

            // The latched stream's drain refuses on close (the healthy
            // stream still drains), and recovery converges over the
            // unchanged active segment so the seal is retryable.
            writer.close().await.unwrap_err();
            let report = store.recover(&log).await.unwrap();
            assert!(
                report.is_empty(),
                "{stream:?}/{site:?}: {:?}",
                report.actions
            );
            let mut writer = store.open_writer(&log).await.unwrap();
            writer.seal(stream).await.unwrap();
            match stream {
                LogStream::Normalized => {
                    assert_eq!(writer.line_watermark(), 1);
                    writer.append_line(2, "two").await.unwrap();
                }
                LogStream::Raw => {
                    assert_eq!(writer.raw_watermark(), b"raw-one".len() as u64);
                    writer.append_raw(b"raw-two").await.unwrap();
                }
            }
            writer.flush().await.unwrap();
            let expected = match stream {
                LogStream::Normalized => b"onetwo".to_vec(),
                LogStream::Raw => b"raw-oneraw-two".to_vec(),
            };
            assert_eq!(
                store.read_committed(&log, stream).await.unwrap(),
                expected,
                "{stream:?}/{site:?}"
            );
            // The other stream kept committing through the latch.
            let (other, other_expected): (LogStream, Vec<u8>) = match stream {
                LogStream::Normalized => (LogStream::Raw, b"raw-continues".to_vec()),
                LogStream::Raw => (LogStream::Normalized, b"norm-continues".to_vec()),
            };
            assert_eq!(
                store.read_committed(&log, other).await.unwrap(),
                other_expected,
                "{stream:?}/{site:?}"
            );
            writer.close().await.unwrap();
        }
    }
}
