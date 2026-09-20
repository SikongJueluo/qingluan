//! Vertical tests of the S2 public storage seam (create/open, append/
//! restart, raw non-UTF-8 round-trip, stream isolation with independent
//! watermarks and segments, committed-prefix visibility at a failpoint,
//! rotation, identity canonicalization, commit-order failpoints, the
//! 64 KiB/50 ms batcher (deadline-driver commits with no following
//! append), and post-I/O-failure latching)
//! — all through `LogStore` and `LogWriter` only, in temp roots that are
//! deleted on exit.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qingluan_core::terminal::{
    ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TerminalId, TerminalRef,
};
use qingluan_storage::{
    AppendOutcome, FaultSite, LogStore, LogStream, ParkPoint, RecoveryAction, StorageError,
    FLUSH_MAX_BYTES, FLUSH_MAX_DELAY, MAX_FRAME_PAYLOAD, MAX_GAP_RECORDS, MAX_SEGMENT_BYTES,
    MAX_SEGMENT_METADATA_ROWS,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temp storage root, removed on drop.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s2-{tag}-{}-{}",
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

fn log_identity(epoch: &str) -> LogIdentity {
    LogIdentity {
        terminal: TerminalRef {
            session: SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("session-1"),
            },
            terminal_id: TerminalId::new("7c9e6679-7425-40de-944b-e07fc1f90ae7"),
        },
        log_epoch: LogEpoch::new(epoch),
    }
}

fn segment_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy().starts_with("seg-")
                    && name.to_string_lossy().ends_with(".log")
            })
        })
        .collect();
    files.sort();
    files
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

/// The segment ids of one stream's live rows, oldest first (through the
/// recovery snapshot, so the assertions stay on the public seam).
async fn segment_ids(store: &LogStore, log: &LogIdentity, stream: LogStream) -> Vec<i64> {
    store
        .recovery_snapshot(log)
        .await
        .unwrap()
        .segments
        .into_iter()
        .filter(|row| row.kind == stream)
        .map(|row| row.segment_id)
        .collect()
}

/// The 50 ms half of the batch policy: with no further append and no
/// explicit flush, the writer's internal flush driver — not a caller
/// — commits the trailing batch. The property under test is *which*
/// mechanism owns the commit, not a latency measurement: the wait is
/// deliberately generous on the real clock (sqlx pool acquire timeouts
/// are tokio timers, which a paused clock's auto-advance fires
/// spuriously) so fsync/SQLite jitter cannot flake it, and the flush
/// bound itself is pinned by `production_policy_constants_are_pinned`.
#[tokio::test]
async fn deadline_driver_commits_a_trailing_batch_with_no_following_append() {
    let root = TempRoot::new("deadline");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    writer.append_line(1, "deadline-line").await.unwrap();
    writer.append_raw(b"deadline-raw").await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        Vec::<u8>::new(),
        "nothing is durable before the boundary"
    );

    // No further append arrives; the driver owns the commit. Errors during
    // the in-flight flush window (file grown past the not-yet-committed
    // boundary) count as "not yet" — the final assert below is outside.
    tokio::time::sleep(FLUSH_MAX_DELAY + std::time::Duration::from_millis(10)).await;
    let mut seen_norm = Vec::new();
    for _ in 0..2000 {
        seen_norm = store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap_or_default();
        if seen_norm == b"deadline-line".to_vec() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let mut seen_raw = Vec::new();
    for _ in 0..2000 {
        seen_raw = store
            .read_committed(&log, LogStream::Raw)
            .await
            .unwrap_or_default();
        if seen_raw == b"deadline-raw".to_vec() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(seen_norm, b"deadline-line".to_vec());
    assert_eq!(seen_raw, b"deadline-raw".to_vec());
    assert_eq!(writer.line_watermark(), 1);
    assert_eq!(writer.raw_watermark(), b"deadline-raw".len() as u64);
    drop(writer);

    // The commit was durable across a fresh open.
    let store = LogStore::open(&root).await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"deadline-line".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"deadline-raw".to_vec()
    );
}
#[tokio::test]
async fn open_creates_database_and_migrations_are_idempotent() {
    let root = TempRoot::new("open");
    let store = LogStore::open(&root).await.unwrap();
    let db = root.join("terminal.db");
    assert!(db.is_file(), "terminal.db must exist after open");
    drop(store);
    // Reopen: migrations are already applied and the format version checks.
    LogStore::open(&root).await.unwrap();
}

#[tokio::test]
async fn production_policy_constants_are_pinned() {
    assert_eq!(MAX_FRAME_PAYLOAD, 64 * 1024);
    assert_eq!(MAX_SEGMENT_BYTES, 4 * 1024 * 1024);
    assert_eq!(FLUSH_MAX_DELAY.as_millis(), 50);
    assert_eq!(FLUSH_MAX_BYTES, 64 * 1024);
    assert_eq!(MAX_SEGMENT_METADATA_ROWS, 64);
    assert_eq!(MAX_GAP_RECORDS, 1024);
}

#[tokio::test]
async fn append_read_roundtrip_and_restart_preserves_epoch_and_watermark() {
    let root = TempRoot::new("roundtrip");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 0);
    assert_eq!(writer.raw_watermark(), 0);

    let appended = writer.append_line(1, "hello 世界").await.unwrap();
    assert_eq!(appended.line, 1);
    assert_eq!(appended.outcome, AppendOutcome::Buffered);
    writer.append_line(2, "").await.unwrap();
    writer
        .append_line(3, "tab\tembedded\u{0}nul")
        .await
        .unwrap();
    let mut expected = "hello 世界".as_bytes().to_vec();
    expected.extend_from_slice(b"");
    expected.extend_from_slice(b"tab\tembedded\x00nul");
    // Only buffered so far: not durable, invisible to reads.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        Vec::<u8>::new(),
        "buffered-only appends must stay invisible"
    );
    // An explicit boundary makes the whole batch durable.
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        expected.clone(),
    );
    // The raw stream stays empty: no raw append ever happened.
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        Vec::<u8>::new()
    );

    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 3);
    assert_eq!(writer.epoch().as_str(), log.log_epoch.as_str());
    writer.append_line(4, "after restart").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);

    let mut expected2 = expected.clone();
    expected2.extend_from_slice(b"after restart");
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        expected2,
    );
    // Restart continues in the same segment (no rotation pressure).
    assert_eq!(segment_files(&root).len(), 1);
}

#[tokio::test]
async fn long_line_spans_frames_as_one_batch_under_64kib_policy() {
    let root = TempRoot::new("longline");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    // > 2 x 64 KiB with multibyte characters straddling both cut points;
    // reaching the 64 KiB batch bound flushes this append synchronously.
    let text = "a".repeat(2 * MAX_FRAME_PAYLOAD - 5) + &"界😀".repeat(40);
    assert!(text.len() > 2 * MAX_FRAME_PAYLOAD);
    let appended = writer.append_line(1, &text).await.unwrap();
    assert_eq!(
        appended.outcome,
        AppendOutcome::Committed,
        "an append reaching the 64 KiB batch bound must flush itself"
    );

    let got = store
        .read_committed(&log, LogStream::Normalized)
        .await
        .unwrap();
    assert_eq!(got, text.as_bytes(), "byte-exact readback across frames");
}

#[tokio::test]
async fn raw_stream_roundtrips_arbitrary_bytes_including_nul_and_invalid_utf8() {
    let root = TempRoot::new("raw-bytes");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    // NUL bytes, invalid UTF-8 (0xff 0xfe, 0xc3 0x28), no line structure.
    let mut expected = Vec::new();
    let first: &[u8] = b"\xff\xfe\x00\x01crash\xc3\x28\x00raw";
    let appended = writer.append_raw(first).await.unwrap();
    assert_eq!(appended.offset, 0);
    assert_eq!(appended.len, first.len() as u64);
    assert_eq!(appended.outcome, AppendOutcome::Buffered);
    writer.flush().await.unwrap();
    expected.extend_from_slice(first);

    // > 2 x 64 KiB with a byte pattern covering every value including 0:
    // the batch bound flushes synchronously and the split is at the byte
    // bound, including inside what would be a multibyte sequence.
    let big: Vec<u8> = (0..2 * MAX_FRAME_PAYLOAD + 10)
        .map(|i| (i % 251) as u8)
        .collect();
    let appended = writer.append_raw(&big).await.unwrap();
    assert_eq!(appended.outcome, AppendOutcome::Committed);
    assert_eq!(appended.offset, first.len() as u64);
    expected.extend_from_slice(&big);

    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        expected,
        "raw bytes must round-trip byte-exact"
    );
    assert_eq!(writer.raw_watermark(), expected.len() as u64);

    // An empty raw append is accepted with `len: 0` at the current
    // offset but buffers no bytes, so the flush boundary finds nothing
    // pending for it: no zero-payload frame is committed, the committed
    // stream is unchanged, and the watermark does not move.
    let empty = writer.append_raw(&[]).await.unwrap();
    assert_eq!(empty.len, 0);
    assert_eq!(empty.offset, writer.raw_watermark());
    writer.flush().await.unwrap();
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        expected
    );

    // Restart preserves the raw watermark; the next append continues it.
    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.raw_watermark(), expected.len() as u64);
    writer.append_raw(b"tail").await.unwrap();
    writer.flush().await.unwrap();
    expected.extend_from_slice(b"tail");
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        expected
    );
}

#[tokio::test]
async fn streams_are_isolated_with_independent_watermarks_and_segments() {
    let root = TempRoot::new("isolation");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    // Interleaved appends: each stream gets its own segments (kind-tagged)
    // and its own sequence; neither ever lands in the other's segment.
    writer.append_line(1, "norm-one").await.unwrap();
    writer.append_raw(b"raw-one").await.unwrap();
    writer.flush().await.unwrap();
    let norm_ids = segment_ids(&store, &log, LogStream::Normalized).await;
    let raw_ids = segment_ids(&store, &log, LogStream::Raw).await;
    assert_eq!(norm_ids.len(), 1);
    assert_eq!(raw_ids.len(), 1);
    assert_ne!(norm_ids[0], raw_ids[0], "streams must not share segments");

    writer.append_line(2, "norm-two").await.unwrap();
    writer.append_raw(b"raw-two").await.unwrap();
    writer.flush().await.unwrap();

    // Watermarks advance independently: lines vs raw bytes.
    assert_eq!(writer.line_watermark(), 2);
    assert_eq!(writer.raw_watermark(), 14);

    // Each stream reads back exactly its own bytes — never the other's.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"norm-onenorm-two".to_vec(),
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-oneraw-two".to_vec(),
    );

    // Restart re-attaches both streams independently and continues both
    // active segments.
    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 2);
    assert_eq!(writer.raw_watermark(), 14);
    writer.append_line(3, "norm-three").await.unwrap();
    writer.append_raw(b"raw-three").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        segment_ids(&store, &log, LogStream::Normalized).await,
        norm_ids,
        "no rotation pressure: the same normalized segment continues"
    );
    assert_eq!(
        segment_ids(&store, &log, LogStream::Raw).await,
        raw_ids,
        "no rotation pressure: the same raw segment continues"
    );
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"norm-onenorm-twonorm-three".to_vec(),
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-oneraw-tworaw-three".to_vec(),
    );
    // Exactly one active segment per stream so far.
    assert_eq!(segment_files(&root).len(), 2);
}

#[tokio::test]
async fn fsynced_uncommitted_normalized_tail_is_invisible_until_commit() {
    let root = TempRoot::new("visibility-norm");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "first").await.unwrap();
    writer.flush().await.unwrap();

    let segment = segment_files(&root)[0].clone();
    let boundary = file_len(&segment);

    // Park the next flushed batch between `sync_data` and the visibility
    // transaction.
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    writer
        .set_park_once(
            ParkPoint::AfterFileSync,
            Box::pin(async move {
                let _ = reached_tx.send(());
                let _ = release_rx.await;
            }),
        )
        .await;
    let mut parked_writer = writer;
    let task = tokio::spawn(async move {
        parked_writer.append_line(2, "second").await.unwrap();
        parked_writer.flush().await
    });
    reached_rx.await.unwrap();

    // The second line's frames are durable on disk but not committed: the
    // file grew past the committed boundary, yet reads see nothing.
    let parked_len = file_len(&segment);
    assert!(parked_len > boundary);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"first".to_vec(),
    );

    // Release: the transaction commits, publication follows, and only then
    // does the line become visible.
    release_tx.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(file_len(&segment), parked_len);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"firstsecond".to_vec(),
    );
}

#[tokio::test]
async fn fsynced_uncommitted_raw_tail_is_invisible_until_commit() {
    let root = TempRoot::new("visibility-raw");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_raw(b"first").await.unwrap();
    writer.flush().await.unwrap();

    let segment = segment_files(&root)[0].clone();
    let boundary = file_len(&segment);

    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    writer
        .set_park_once(
            ParkPoint::AfterFileSync,
            Box::pin(async move {
                let _ = reached_tx.send(());
                let _ = release_rx.await;
            }),
        )
        .await;
    let mut parked_writer = writer;
    let task = tokio::spawn(async move {
        parked_writer.append_raw(b"\xffsecond\x00").await.unwrap();
        parked_writer.flush().await
    });
    reached_rx.await.unwrap();

    let parked_len = file_len(&segment);
    assert!(parked_len > boundary);
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"first".to_vec(),
        "fsynced-but-uncommitted raw bytes must stay invisible"
    );

    release_tx.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(file_len(&segment), parked_len);
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"first\xffsecond\x00".to_vec(),
    );
}

#[tokio::test]
async fn uncommitted_tail_on_restart_requires_recovery() {
    let root = TempRoot::new("restart-tail");
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    // Seed the committed prefix with a writer that shuts down cleanly.
    {
        let store = LogStore::open(&root).await.unwrap();
        let mut writer = store.open_writer(&log).await.unwrap();
        writer.append_line(1, "first").await.unwrap();
        writer.flush().await.unwrap();
        writer.close().await.unwrap();
    }
    let segment = segment_files(&root)[0].clone();
    let boundary = file_len(&segment);

    // The writer that dies mid-flush runs on its own runtime in its own
    // thread — the abrupt-death equivalent of a kill between fsync and
    // commit. (Aborting the caller's task no longer kills a foreground
    // flush: its command task is detached by design and would finish the
    // commit, so the crash is simulated by tearing the whole runtime
    // down while the flush is parked.)
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let root_path = root.to_path_buf();
    let dying_log = log.clone();
    let dying = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let store = LogStore::open(&root_path).await.unwrap();
            let mut writer = store.open_writer(&dying_log).await.unwrap();
            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
            writer
                .set_park_once(
                    ParkPoint::AfterFileSync,
                    Box::pin(async move {
                        let _ = reached_tx.send(());
                        // Never released: the writer dies between fsync and
                        // commit.
                        std::future::pending::<()>().await;
                    }),
                )
                .await;
            let _appending = tokio::spawn(async move {
                let _ = writer.append_line(2, "second").await;
                let _ = writer.flush().await;
            });
            // Drive this runtime until the flush is parked just past its
            // fsync, then return: the runtime drops at the end of this
            // block, killing the parked flush exactly where it stands —
            // after the sync, before the visibility transaction.
            let _ = reached_rx.await;
            let _ = parked_tx.send(());
            drop(store);
        });
    });
    parked_rx.recv().unwrap();
    let parked_len = file_len(&segment);
    assert!(parked_len > boundary);
    // "Kill" the writer between fsync and commit: the runtime (and every
    // task spawned on it) is torn down before the thread joins.
    dying.join().unwrap();

    let store = LogStore::open(&root).await.unwrap();
    // A writer start must refuse the uncommitted tail until recovery ran
    // (assert, don't repair).
    match store.open_writer(&log).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("committed boundary"), "{detail}");
        }
        other => panic!(
            "expected RecoveryRequired, got {:?}",
            other.map(|_| ()).map_err(|error| error.to_string())
        ),
    }
    // Committed-prefix reads keep working; the parked bytes stay excluded.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"first".to_vec(),
    );

    // Recovery quarantines the tail as an artifact before truncating, then
    // a rerun is a zero-action pass over an equal durable state, and the
    // writer continues with unreused numbering.
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::TailQuarantined {
            file_name: segment.file_name().unwrap().to_string_lossy().into_owned(),
            quarantined_bytes: parked_len - boundary,
            truncated_to: boundary,
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let rerun = store.recover(&log).await.unwrap();
    assert!(rerun.is_empty(), "second recovery must take zero actions");
    assert_eq!(
        store.recovery_snapshot(&log).await.unwrap(),
        snapshot,
        "durable state must be equal after the second recovery"
    );
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 1);
    writer.append_line(2, "second").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"firstsecond".to_vec(),
    );
}

#[tokio::test]
async fn append_line_enforces_line_sequence_independently_of_raw() {
    let root = TempRoot::new("sequence");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    assert!(matches!(
        writer.append_line(1, "reuse").await,
        Err(StorageError::LineNotSequential {
            attempted: 1,
            watermark: 1
        })
    ));
    assert!(matches!(
        writer.append_line(3, "skip").await,
        Err(StorageError::LineNotSequential {
            attempted: 3,
            watermark: 1
        })
    ));
    assert_eq!(writer.line_watermark(), 1);
    // Line sequencing never constrains the raw stream.
    let raw = writer.append_raw(b"unaffected").await.unwrap();
    assert_eq!(raw.offset, 0);
    writer.append_line(2, "two").await.unwrap();
    assert_eq!(writer.line_watermark(), 2);
    assert_eq!(writer.raw_watermark(), 10);
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec(),
    );
}

#[tokio::test]
async fn identity_must_be_canonical_uuid_text_before_any_mutation() {
    let root = TempRoot::new("identity");
    let store = LogStore::open(&root).await.unwrap();
    let good = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");

    let mut bad_terminal = good.clone();
    bad_terminal.terminal.terminal_id = TerminalId::new("terminal-one");
    assert!(matches!(
        store.open_writer(&bad_terminal).await,
        Err(StorageError::InvalidIdentity {
            field: "terminal_id",
            ..
        })
    ));

    let mut bad_epoch = good.clone();
    bad_epoch.log_epoch = LogEpoch::new("epoch-1");
    assert!(matches!(
        store.open_writer(&bad_epoch).await,
        Err(StorageError::InvalidIdentity {
            field: "log_epoch",
            ..
        })
    ));

    // Non-canonical spellings of one UUID must be rejected, so the text
    // key and the 16-byte header identity stay strictly 1:1.
    for spelling in [
        "7C9E6679-7425-40DE-944B-E07FC1F90AE7",          // upper case
        "{7c9e6679-7425-40de-944b-e07fc1f90ae7}",        // braced
        "urn:uuid:7c9e6679-7425-40de-944b-e07fc1f90ae7", // URN
        "7c9e6679742540de944be07fc1f90ae7",              // simple (no hyphens)
    ] {
        let mut aliased = good.clone();
        aliased.terminal.terminal_id = TerminalId::new(spelling);
        assert!(
            matches!(
                store.open_writer(&aliased).await,
                Err(StorageError::InvalidIdentity { .. })
            ),
            "non-canonical spelling {spelling:?} must be rejected"
        );
    }

    // No residue: no segment file was created for rejected identities.
    assert!(segment_files(&root).is_empty());
    store.open_writer(&good).await.unwrap();
}

#[tokio::test]
async fn epoch_mismatch_is_typed_not_silent() {
    let root = TempRoot::new("epoch");
    let store = LogStore::open(&root).await.unwrap();
    let epoch_a = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&epoch_a).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    drop(writer);

    let epoch_b = log_identity("1b2c3d4e-0f0f-4a1b-8c2d-0e0f4a1b8c2e");
    assert!(matches!(
        store.open_writer(&epoch_b).await,
        Err(StorageError::EpochMismatch { .. })
    ));
    // Reads with the wrong epoch are rejected too, never re-anchored.
    assert!(matches!(
        store.read_committed(&epoch_b, LogStream::Normalized).await,
        Err(StorageError::EpochMismatch { .. })
    ));
    // The original epoch still opens (ordinary restart preserves it).
    store.open_writer(&epoch_a).await.unwrap();
}

#[tokio::test]
async fn terminal_uuid_mismatch_is_typed_not_silent() {
    // Canonical parsing keeps the text key and header identity 1:1, so a
    // mismatch can only come from a tampered or restored database — and
    // then attach must refuse instead of adopting someone else's segments.
    let root = TempRoot::new("terminal-uuid");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    drop(store);

    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.join("terminal.db"));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query("UPDATE terminal SET terminal_uuid = ?1")
        .bind([9u8; 16].as_slice())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.open_writer(&log).await {
        Err(StorageError::TerminalUuidMismatch { stored, .. }) => {
            assert_eq!(stored, uuid::Uuid::from_bytes([9u8; 16]).to_string());
        }
        other => panic!(
            "expected TerminalUuidMismatch, got {:?}",
            other.map(|_| ()).map_err(|error| error.to_string())
        ),
    }
}

#[tokio::test]
async fn seal_forces_a_fresh_segment_per_stream() {
    let root = TempRoot::new("seal");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.append_raw(b"r1").await.unwrap();
    writer.flush().await.unwrap();
    writer.seal(LogStream::Normalized).await.unwrap();
    // Sealing one stream leaves the other untouched (sealing also flushes
    // the sealed stream's pending batch into the segment it closes).
    writer.append_raw(b"r2").await.unwrap();
    writer.flush().await.unwrap();
    let raw_ids = segment_ids(&store, &log, LogStream::Raw).await;
    writer.append_line(2, "two").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        segment_ids(&store, &log, LogStream::Normalized).await.len(),
        2,
        "a sealed stream's next append creates a fresh segment"
    );
    assert_eq!(
        segment_ids(&store, &log, LogStream::Raw).await,
        raw_ids,
        "sealing one stream never rotates the other"
    );
    assert_eq!(segment_files(&root).len(), 3);

    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec(),
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"r1r2".to_vec(),
    );

    // Restart attaches the second (active) normalized segment and the
    // still-active raw segment.
    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(3, "three").await.unwrap();
    writer.append_raw(b"r3").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        segment_ids(&store, &log, LogStream::Normalized).await.len(),
        2
    );
    assert_eq!(segment_ids(&store, &log, LogStream::Raw).await, raw_ids);
}

#[tokio::test]
async fn rotation_across_three_segments_reads_byte_exact() {
    let root = TempRoot::new("rotation");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    let line = |n: u64| {
        let prefix = format!("line-{n}-");
        let mut text = prefix;
        text.push_str(&"x".repeat(3 * 1024 * 1024));
        text
    };
    let texts = [line(1), line(2), line(3)];
    for (i, text) in texts.iter().enumerate() {
        writer
            .append_line(u64::try_from(i + 1).unwrap(), text)
            .await
            .unwrap();
        writer.flush().await.unwrap();
    }
    let files = segment_files(&root);
    assert!(
        files.len() >= 3,
        "three ~3 MiB lines must rotate into three segments, got {}",
        files.len()
    );
    let ids = segment_ids(&store, &log, LogStream::Normalized).await;
    assert!(ids.windows(2).all(|w| w[0] < w[1]));

    let expected: Vec<u8> = texts.concat().into_bytes();
    let got = store
        .read_committed(&log, LogStream::Normalized)
        .await
        .unwrap();
    assert_eq!(got.len(), expected.len());
    assert_eq!(got, expected, "byte-exact across segment boundaries");
}

#[tokio::test]
async fn raw_rotation_seals_at_max_segment_bytes() {
    let root = TempRoot::new("raw-rotation");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    let chunk: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 256) as u8).collect();
    let mut expected = Vec::new();
    for _ in 0..3 {
        writer.append_raw(&chunk).await.unwrap();
        writer.flush().await.unwrap();
        expected.extend_from_slice(&chunk);
    }
    let ids = segment_ids(&store, &log, LogStream::Raw).await;
    assert!(
        ids.windows(2).all(|w| w[0] != w[1]),
        "3 MiB raw appends must rotate into distinct segments"
    );
    assert_eq!(writer.raw_watermark(), expected.len() as u64);
    let got = store.read_committed(&log, LogStream::Raw).await.unwrap();
    assert_eq!(got.len(), expected.len());
    assert_eq!(got, expected, "byte-exact raw readback across rotation");
}

#[tokio::test]
async fn commit_sequence_failpoints_fire_in_durability_order() {
    use qingluan_storage::{CrashPoint, CrashSink};
    use std::sync::{Arc, Mutex};

    let root = TempRoot::new("failpoints");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    let seen: std::sync::Arc<Mutex<Vec<CrashPoint>>> = std::sync::Arc::default();
    let recorder = Arc::clone(&seen);
    writer.set_crash_sink(Some(Arc::new(move |point: CrashPoint| {
        recorder.lock().unwrap().push(point);
    }) as CrashSink));
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();

    // The production ordering: header synced + dir fsynced before any
    // frame; frames written then synced before the transaction; the
    // transaction runs its statements then commits; publication last.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            CrashPoint::SegmentBefore,
            CrashPoint::SegmentRowInserted,
            CrashPoint::SegmentHeaderWritten,
            CrashPoint::SegmentHeaderSynced,
            CrashPoint::FrameBeforeWrite,
            CrashPoint::FrameAfterWrite,
            CrashPoint::FrameAfterSync,
            CrashPoint::TxnBegin,
            CrashPoint::TxnUpdate,
            CrashPoint::TxnBeforeCommit,
            CrashPoint::TxnAfterCommit,
            CrashPoint::PublishBefore,
            CrashPoint::PublishAfter,
        ]
    );

    // The raw stream follows the identical ordering.
    seen.lock().unwrap().clear();
    writer.append_raw(b"raw").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            CrashPoint::SegmentBefore,
            CrashPoint::SegmentRowInserted,
            CrashPoint::SegmentHeaderWritten,
            CrashPoint::SegmentHeaderSynced,
            CrashPoint::FrameBeforeWrite,
            CrashPoint::FrameAfterWrite,
            CrashPoint::FrameAfterSync,
            CrashPoint::TxnBegin,
            CrashPoint::TxnUpdate,
            CrashPoint::TxnBeforeCommit,
            CrashPoint::TxnAfterCommit,
            CrashPoint::PublishBefore,
            CrashPoint::PublishAfter,
        ]
    );
}

/// The 64 KiB half of the batch policy: the append that reaches the byte
/// bound flushes itself (both streams, independently).
#[tokio::test]
async fn size_bound_flushes_the_reaching_append_synchronously() {
    let root = TempRoot::new("size-bound");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();

    // Below the bound: buffered only, time has not moved.
    let text = "x".repeat(FLUSH_MAX_BYTES - 1);
    let appended = writer.append_line(1, &text).await.unwrap();
    assert_eq!(appended.outcome, AppendOutcome::Buffered);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        Vec::<u8>::new()
    );

    // Reaching the bound: this append flushes the whole batch.
    let big = "y".repeat(FLUSH_MAX_BYTES);
    let appended = writer.append_line(2, &big).await.unwrap();
    assert_eq!(appended.outcome, AppendOutcome::Committed);
    let mut expected = text.into_bytes();
    expected.extend_from_slice(&big.into_bytes());
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        expected
    );

    // The raw stream flushes independently at its own bound.
    let bytes = vec![b'r'; FLUSH_MAX_BYTES];
    let appended = writer.append_raw(&bytes).await.unwrap();
    assert_eq!(appended.outcome, AppendOutcome::Committed);
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        bytes
    );
    drop(writer);
}

fn require_recovery(result: Result<impl std::fmt::Debug, StorageError>) -> String {
    match result {
        Err(StorageError::RecoveryRequired { detail }) => detail,
        other => panic!("expected RecoveryRequired, got {:?}", other.err()),
    }
}

/// Gate C's append/sync/commit-failure behavior: once frame bytes of a
/// flushed batch may exist beyond the committed boundary, every later
/// append, flush, and seal of that stream refuses with a typed
/// `RecoveryRequired` until recovery truncated the tail; the retry after
/// recovery succeeds with unreused numbering.
async fn post_failure_latch_case(site: FaultSite, expect_io: bool) {
    let root = TempRoot::new("fault-latch");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity("0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d");
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "first").await.unwrap();
    writer.flush().await.unwrap();

    // Inject the failure into the next flushed batch: the append buffers,
    // and the fault fires when that batch walks the commit sequence.
    writer.set_fault_once(site).await;
    writer
        .append_line(2, "second")
        .await
        .expect("the append itself only buffers");
    match writer.flush().await {
        Err(error) => {
            if expect_io {
                assert!(matches!(error, StorageError::Io { .. }), "{error}");
            } else {
                assert!(matches!(error, StorageError::Database(_)), "{error}");
            }
        }
        Ok(outcomes) => panic!("injected fault must fail the flushed batch, got {outcomes:?}"),
    }

    // Every further use of the poisoned stream refuses until recovery;
    // the raw stream is untouched by the normalized failure (flush
    // attempts both streams and reports the first error).
    for detail in [
        require_recovery(writer.append_line(2, "second").await.map(|_| ())),
        require_recovery(writer.flush().await),
        require_recovery(writer.seal(LogStream::Normalized).await),
    ] {
        assert!(detail.contains("recovery"), "{detail}");
    }
    writer.append_raw(b"raw-continues").await.unwrap();
    let _ = writer.flush().await; // raw commits, normalized still refuses

    // Recovery converges: the uncommitted tail (whatever of it reached the
    // file) is quarantined, numbering continues unreused.
    drop(writer);
    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert!(
        report
            .actions
            .iter()
            .all(|action| matches!(action, RecoveryAction::TailQuarantined { .. })),
        "{:?}",
        report.actions
    );
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 1);
    writer.append_line(2, "second").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"firstsecond".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-continues".to_vec()
    );
}

#[tokio::test]
async fn partial_frame_write_latches_recovery_required_until_recovered() {
    post_failure_latch_case(FaultSite::PartialFrameWrite, true).await;
}

#[tokio::test]
async fn sync_failure_latches_recovery_required_until_recovered() {
    post_failure_latch_case(FaultSite::FrameSync, true).await;
}

#[tokio::test]
async fn commit_failure_latches_recovery_required_until_recovered() {
    post_failure_latch_case(FaultSite::Commit, false).await;
}
