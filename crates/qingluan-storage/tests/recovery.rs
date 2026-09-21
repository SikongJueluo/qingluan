//! Recovery and retention through the public seam: uncommitted-tail
//! quarantine (artifact before truncation), zero-committed-row
//! tombstones, whole-orphan quarantine, stream-scoped gaps with
//! degradation and preserved watermarks, epoch rules (normal restart and
//! rotation keep it; a destructive database rebuild changes it and
//! rejects the old identity), the 64-row combined segment budget with
//! retained-floor advance and the unsafe-reclaim latch, gap coalescing,
//! and propagation of repair I/O errors. Idempotence is asserted as
//! zero actions plus an equal durable snapshot on the second run.

use std::path::{Path, PathBuf};

use qingluan_core::terminal::{
    ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TerminalId, TerminalRef,
};
use qingluan_storage::{
    AppendOutcome, FLUSH_MAX_BYTES, GapReason, LogStore, LogStream, MAX_GAP_RECORDS,
    MAX_SEGMENT_METADATA_ROWS, RecoveryAction, StorageError,
};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s2b-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
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

const TERMINAL: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const EPOCH_A: &str = "0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d";
const EPOCH_B: &str = "1b2c3d4e-0f0f-4a1b-8c2d-0e0f4a1b8c2e";

fn log_identity(epoch: &str) -> LogIdentity {
    LogIdentity {
        terminal: TerminalRef {
            session: SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("session-1"),
            },
            terminal_id: TerminalId::new(TERMINAL),
        },
        log_epoch: LogEpoch::new(epoch),
    }
}

fn seg_file(root: &Path, id: i64) -> PathBuf {
    root.join(format!("seg-{id:06}.log"))
}

fn segment_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("seg-") && name.ends_with(".log")
            })
        })
        .collect();
    files.sort();
    files
}

/// Two sealed normalized segments (lines 1-2 and 3-4) plus the raw stream
/// seeded with one chunk, writer detached (all batches flushed).
async fn seeded(root: &TempRoot, log: &LogIdentity) -> LogStore {
    let store = LogStore::open(root).await.unwrap();
    let mut writer = store.open_writer(log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.append_line(2, "two").await.unwrap();
    writer.seal(LogStream::Normalized).await.unwrap();
    writer.append_line(3, "three").await.unwrap();
    writer.append_line(4, "four").await.unwrap();
    writer.append_raw(b"raw-chunk").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    store
}

async fn raw_pool(root: &Path) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.join("terminal.db"));
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// A second recovery must be a zero-action pass over an equal durable
/// state, and the given first-run report must hold.
async fn assert_idempotent(store: &LogStore, log: &LogIdentity) {
    let snapshot = store.recovery_snapshot(log).await.unwrap();
    let rerun = store.recover(log).await.unwrap();
    assert!(
        rerun.actions.is_empty(),
        "second recovery took actions: {:?}",
        rerun.actions
    );
    assert_eq!(
        store.recovery_snapshot(log).await.unwrap(),
        snapshot,
        "durable state changed on the second recovery"
    );
}

#[tokio::test]
async fn junk_and_torn_tails_are_quarantined_then_truncated() {
    for tail in [&b"junk-garbage-tail"[..], &b"QLFR\x01\x00"[..]] {
        let root = TempRoot::new("tail");
        let log = log_identity(EPOCH_A);
        let store = seeded(&root, &log).await;
        drop(store);
        let path = seg_file(&root, 1);
        let original = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, tail).unwrap();
        drop(file);

        let store = LogStore::open(&root).await.unwrap();
        let report = store.recover(&log).await.unwrap();
        assert_eq!(
            report.actions,
            vec![RecoveryAction::TailQuarantined {
                file_name: "seg-000001.log".into(),
                quarantined_bytes: tail.len() as u64,
                truncated_to: original,
            }],
            "tail {tail:?}"
        );
        // The artifact holds the quarantined bytes; the live file is back at
        // its committed boundary.
        let artifact = std::fs::read(root.join("quarantine-seg-000001.log")).unwrap();
        assert_eq!(artifact, tail);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), original);
        // Degradation is not latched: no committed data was lost.
        let snapshot = store.recovery_snapshot(&log).await.unwrap();
        assert!(!snapshot.degraded);
        assert_eq!(snapshot.line_watermark, 4);
        assert_idempotent(&store, &log).await;
    }
}

#[tokio::test]
async fn zero_committed_rows_are_tombstoned_whole_and_never_adopted() {
    let root = TempRoot::new("tombstone");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);

    let pool = raw_pool(&root).await;
    sqlx::query(
        "INSERT INTO segment
             (segment_id, session_source, external_session_id, terminal_id, kind, file_name,
              first_line, last_line, first_offset, last_offset, committed_bytes,
              fsynced_bytes, state, created_ms)
         VALUES (900001, 'pi', 'session-1', ?1, 'normalized', 'seg-900001.log', 9, 9, 0, 0,
                 0, 0, 'active', 0),
                (900002, 'pi', 'session-1', ?1, 'raw', 'seg-900002.log', NULL, NULL, 4096,
                 4096, 0, 0, 'active', 0),
                (900005, 'pi', 'session-1', ?1, 'normalized', 'seg-900005.log', 11, 11, 0, 0,
                 0, 0, 'active', 0)",
    )
    .bind(TERMINAL)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    // A garbage partial file for the second crashed creation, and a
    // *header-valid* file (a copy of a real segment of this epoch) for the
    // third: neither may ever be adopted.
    std::fs::write(seg_file(&root, 900_002), b"partial-header").unwrap();
    std::fs::copy(seg_file(&root, 1), seg_file(&root, 900_005)).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![
            RecoveryAction::ZeroByteRowTombstoned {
                segment_id: 900_001
            },
            RecoveryAction::ZeroByteRowTombstoned {
                segment_id: 900_002
            },
            RecoveryAction::ZeroByteRowTombstoned {
                segment_id: 900_005
            },
        ]
    );
    // The garbage partial file and the header-valid pending file are both
    // quarantined whole, never adopted.
    assert!(root.join("quarantine-seg-900002.log").is_file());
    assert!(root.join("quarantine-seg-900005.log").is_file());
    assert!(!seg_file(&root, 900_002).exists());
    assert!(!seg_file(&root, 900_005).exists());
    // No gap: nothing committed was lost, and degradation is not latched.
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.gaps.is_empty());
    assert!(!snapshot.degraded);
    assert_eq!(snapshot.segments.len(), 3);
    assert_idempotent(&store, &log).await;
    // Writing continues in the surviving segments.
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
}

#[tokio::test]
async fn orphan_segment_files_are_quarantined_whole() {
    let root = TempRoot::new("orphan");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // An unattributed duplicate of a real segment: the header parses to this
    // terminal, but no row claims the file.
    std::fs::copy(seg_file(&root, 1), seg_file(&root, 900_003)).unwrap();
    // Unparseable garbage on a storage-owned name.
    std::fs::write(seg_file(&root, 900_004), b"not a segment").unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-900003.log".into()
            },
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-900004.log".into()
            },
        ]
    );
    assert!(root.join("quarantine-seg-900003.log").is_file());
    assert!(root.join("quarantine-seg-900004.log").is_file());
    // The claimed segments are untouched and fully readable.
    assert_eq!(segment_files(&root).len(), 3);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwothreefour".to_vec()
    );
    assert_idempotent(&store, &log).await;
}

enum Damage {
    Remove,
    Truncate,
    Corrupt,
}

async fn indexed_segment_damage_case(damage: Damage) {
    let root = TempRoot::new("damage");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);

    let path = seg_file(&root, 1);
    let committed = std::fs::metadata(&path).unwrap().len();
    match damage {
        Damage::Remove => std::fs::remove_file(&path).unwrap(),
        Damage::Truncate => {
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_len(committed - 1).unwrap();
        }
        Damage::Corrupt => {
            let mut data = std::fs::read(&path).unwrap();
            data[(committed - 8) as usize] ^= 0xFF;
            std::fs::write(&path, data).unwrap();
        }
    }

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    let reason = match damage {
        Damage::Remove => GapReason::Missing,
        Damage::Truncate => GapReason::Truncated,
        Damage::Corrupt => GapReason::Corrupt,
    };
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Normalized,
            range_start: 1,
            range_end: 3,
            reason,
            file_name: "seg-000001.log".into(),
        }]
    );

    // Explicit stream-scoped gap + degraded; the watermark is preserved
    // (never decreased) and numbering continues without reuse.
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.line_watermark, 4);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Normalized);
    assert_eq!(snapshot.gaps[0].start, 1);
    assert_eq!(snapshot.gaps[0].end, 3);
    assert_eq!(snapshot.gaps[0].reason, reason);
    // The surviving segment reads back; the raw stream is unaffected.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"threefour".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk".to_vec()
    );
    // Degraded does not block writing: line 5 continues the numbering.
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 4);
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"threefourfive".to_vec()
    );
    drop(writer);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn missing_indexed_segment_is_an_explicit_gap_with_degradation() {
    indexed_segment_damage_case(Damage::Remove).await;
}

#[tokio::test]
async fn truncated_indexed_segment_is_an_explicit_gap_with_degradation() {
    indexed_segment_damage_case(Damage::Truncate).await;
}

#[tokio::test]
async fn corrupt_indexed_segment_is_an_explicit_gap_with_degradation() {
    indexed_segment_damage_case(Damage::Corrupt).await;
}

#[tokio::test]
async fn lost_active_segment_clears_its_pointer_and_continues_numbering() {
    let root = TempRoot::new("active-lost");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // Segment 2 is the active normalized segment (lines 3-4).
    std::fs::remove_file(seg_file(&root, 2)).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Normalized,
            range_start: 3,
            range_end: 5,
            reason: GapReason::Missing,
            file_name: "seg-000002.log".into(),
        }]
    );
    // The pointer went with the tombstone in the same transaction.
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(snapshot.line_watermark, 4);
    assert!(snapshot.degraded);
    // Writing continues in a fresh segment anchored at watermark + 1.
    let mut writer = store.open_writer(&log).await.unwrap();
    let committed = writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(committed.line, 5);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwofive".to_vec()
    );
    drop(writer);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn adjacent_losses_of_one_stream_coalesce_into_one_gap_record() {
    let root = TempRoot::new("coalesce");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    std::fs::remove_file(seg_file(&root, 1)).unwrap();
    std::fs::remove_file(seg_file(&root, 2)).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(report.actions.len(), 2);
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    // [1,3) + [3,5) coalesce to one record [1,5); the raw stream records
    // nothing.
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Normalized);
    assert_eq!(snapshot.gaps[0].start, 1);
    assert_eq!(snapshot.gaps[0].end, 5);
    assert_eq!(snapshot.line_watermark, 4);
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn raw_stream_losses_are_gapped_in_byte_coordinates_only() {
    let root = TempRoot::new("raw-gap");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    std::fs::remove_file(seg_file(&root, 3)).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Raw,
            range_start: 0,
            range_end: 9,
            reason: GapReason::Missing,
            file_name: "seg-000003.log".into(),
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Raw);
    assert_eq!(snapshot.gaps[0].start, 0);
    assert_eq!(snapshot.gaps[0].end, 9);
    assert!(snapshot.degraded);
    // The normalized stream keeps reading fully; its watermark is intact
    // and the raw watermark never decreased.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwothreefour".to_vec()
    );
    assert_eq!(snapshot.raw_watermark, 9);
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_raw(b"next").await.unwrap();
    writer.flush().await.unwrap();
    assert_eq!(writer.raw_watermark(), 13);
    drop(writer);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn destructive_rebuild_changes_epoch_rejects_old_identity_and_quarantines_files() {
    let root = TempRoot::new("destructive");
    let old = log_identity(EPOCH_A);
    let store = seeded(&root, &old).await;
    drop(store);
    assert!(seg_file(&root, 1).is_file());

    // Destructive rebuild: delete the database with its WAL sidecars.
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(root.db().with_file_name(format!("terminal.db{suffix}")));
    }

    let store = LogStore::open(&root).await.unwrap();
    let rebuilt = log_identity(EPOCH_B);
    let report = store.recover(&rebuilt).await.unwrap();
    assert_eq!(
        report.actions,
        vec![
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-000001.log".into()
            },
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-000002.log".into()
            },
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-000003.log".into()
            },
        ],
        "old-epoch files are quarantined whole, never adopted"
    );
    let snapshot = store.recovery_snapshot(&rebuilt).await.unwrap();
    assert_eq!(snapshot.line_watermark, 0);
    assert!(snapshot.segments.is_empty());
    assert!(!snapshot.degraded);

    // The new epoch writes from scratch; numbering restarts (the recorded
    // destructive cost). The fresh database mints its own segment ids, so
    // the new file may reuse a quarantined name without touching the
    // artifact.
    let mut writer = store.open_writer(&rebuilt).await.unwrap();
    writer.append_line(1, "rebuilt").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_eq!(
        store
            .read_committed(&rebuilt, LogStream::Normalized)
            .await
            .unwrap(),
        b"rebuilt".to_vec()
    );

    // The old epoch identity is rejected, never silently adopted.
    assert!(matches!(
        store.open_writer(&old).await,
        Err(StorageError::EpochMismatch { .. })
    ));
    assert!(matches!(
        store.read_committed(&old, LogStream::Normalized).await,
        Err(StorageError::EpochMismatch { .. })
    ));
    assert!(root.join("quarantine-seg-000001.log").is_file());
    assert_idempotent(&store, &rebuilt).await;
}

#[tokio::test]
async fn repair_io_errors_propagate_and_recovery_converges_after() {
    let root = TempRoot::new("io-error");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // Simulate an uncommitted tail, then make the storage root read-only:
    // persisting the quarantine artifact must fail loudly (its file sync
    // and directory sync errors propagate), never silently skip.
    let path = seg_file(&root, 1);
    let original = std::fs::metadata(&path).unwrap().len();
    let junk = b"tail-that-cannot-be-quarantined";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, junk).unwrap();
    drop(file);

    let store = LogStore::open(&root).await.unwrap();
    let mut permissions = std::fs::metadata(&*root).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
    std::fs::set_permissions(&*root, permissions).unwrap();
    match store.recover(&log).await {
        Err(StorageError::Io { .. }) => {}
        other => panic!(
            "expected propagated Io error, got {:?}",
            other.map(|_| ()).map_err(|error| error.to_string())
        ),
    }
    // The live file is untouched: truncation never ran ahead of the
    // artifact.
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        original + junk.len() as u64
    );

    let mut permissions = std::fs::metadata(&*root).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&*root, permissions).unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(report.actions.len(), 1);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), original);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn sixty_fifth_segment_reclaims_oldest_sealed_and_advances_retained_position() {
    let root = TempRoot::new("retention");
    let log = log_identity(EPOCH_A);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    // Test-only rotation threshold: the budget semantics are what is
    // verified, not the 4 MiB byte boundary (covered by the rotation
    // tests in the seam suite).
    writer.set_rotation_threshold(2048).await;

    let mut normalized: Vec<u8> = Vec::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut line = 0u64;
    for round in 0..80 {
        let text = format!("line-{round:03}-") + &"x".repeat(1400);
        line += 1;
        writer.append_line(line, &text).await.unwrap();
        writer.flush().await.unwrap();
        normalized.extend_from_slice(text.as_bytes());
        let chunk: Vec<u8> = vec![b'r'; 1400];
        writer.append_raw(&chunk).await.unwrap();
        writer.flush().await.unwrap();
        raw.extend_from_slice(&chunk);
        // The combined raw+normalized budget holds at every creation.
        let snapshot = store.recovery_snapshot(&log).await.unwrap();
        assert!(
            snapshot.segments.len() <= MAX_SEGMENT_METADATA_ROWS,
            "round {round}: {} live rows exceed the budget",
            snapshot.segments.len()
        );
    }

    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(
        snapshot.segments.len(),
        MAX_SEGMENT_METADATA_ROWS,
        "steady state sits exactly at the budget"
    );
    // Both kinds share one budget: reclaim took the oldest rows overall.
    let normalized_rows = snapshot
        .segments
        .iter()
        .filter(|row| row.kind == LogStream::Normalized)
        .count();
    let raw_rows = snapshot.segments.len() - normalized_rows;
    assert!(normalized_rows > 0 && raw_rows > 0);
    // The retained floors advanced past the reclaimed prefixes and the
    // on-disk segment files match the live rows exactly.
    assert!(snapshot.retained_first_line > 1, "line floor advanced");
    assert!(
        snapshot.retained_first_offset > 0,
        "raw floor advanced once normalized rows were exhausted"
    );
    assert_eq!(segment_files(&root).len(), snapshot.segments.len());
    // No gaps were recorded: reclamation is expiry (a retained floor),
    // never a fabricated loss.
    assert!(snapshot.gaps.is_empty());
    assert!(!snapshot.degraded);

    // Reads return exactly the retained range in both streams.
    let line_len = "line-000-".len() + 1400;
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        normalized[line_len * (snapshot.retained_first_line as usize - 1)..],
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        raw[snapshot.retained_first_offset as usize..],
    );

    // Rotation keeps the epoch; restart keeps it too, and writing
    // continues without reuse.
    let epoch_before = writer.epoch().clone();
    drop(writer);
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(*writer.epoch(), epoch_before);
    line += 1;
    writer.append_line(line, "post-budget").await.unwrap();
    writer.flush().await.unwrap();
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(snapshot.line_watermark, line);
    assert_eq!(snapshot.segments.len(), MAX_SEGMENT_METADATA_ROWS);
}

#[tokio::test]
async fn cross_stream_active_pointer_is_cleared_by_recovery() {
    let root = TempRoot::new("pointer-tamper");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // Tamper the normalized pointer onto the raw stream's segment: the
    // two streams must never share a pointer.
    let pool = raw_pool(&root).await;
    sqlx::query(
        "UPDATE terminal
            SET active_normalized_segment =
                (SELECT segment_id FROM segment WHERE kind = 'raw' ORDER BY segment_id LIMIT 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::ActivePointerCleared {
            stream: LogStream::Normalized
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert_eq!(snapshot.active_normalized_segment, None);
    assert_eq!(snapshot.active_raw_segment, Some(3));
    // Writing continues: a fresh normalized segment, unchanged numbering.
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_idempotent(&store, &log).await;
}

#[tokio::test]
async fn unsafe_reclaim_drops_batches_as_gaps_and_keeps_the_writer_draining() {
    let root = TempRoot::new("unsafe-reclaim");
    let log = log_identity(EPOCH_A);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.set_rotation_threshold(2048).await;
    let text = || format!("line-{}-", "x".repeat(1400));
    // A >= 64 KiB batch flushes itself, so the drop outcome (if any)
    // surfaces on the append that triggered it.
    let draining_text = || "d".repeat(FLUSH_MAX_BYTES);
    loop {
        writer
            .append_line(writer.line_watermark() + 1, &text())
            .await
            .unwrap();
        writer.flush().await.unwrap();
        if store.recovery_snapshot(&log).await.unwrap().segments.len() == MAX_SEGMENT_METADATA_ROWS
        {
            break;
        }
    }
    let committed_prefix = store
        .read_committed(&log, LogStream::Normalized)
        .await
        .unwrap();
    let watermark_before = writer.line_watermark();
    drop(writer);
    // Tamper the budget into an unsafe shape: no row is sealed, so the
    // 65th creation has nothing it may reclaim.
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE segment SET state = 'active'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert!(
        report.is_empty(),
        "tampered-but-consistent rows need no repair"
    );
    // The already-running writer keeps draining: the append that cannot be
    // given a segment is dropped as an explicit stream-scoped gap with its
    // numbers consumed (never reused) instead of failing the producer.
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.set_rotation_threshold(2048).await;
    let dropped_one = writer
        .append_line(watermark_before + 1, &draining_text())
        .await
        .unwrap();
    assert_eq!(dropped_one.line, watermark_before + 1);
    assert_eq!(dropped_one.outcome, AppendOutcome::Dropped);
    assert_eq!(writer.line_watermark(), watermark_before + 1);
    let dropped_two = writer
        .append_line(watermark_before + 2, &draining_text())
        .await
        .unwrap();
    assert_eq!(dropped_two.outcome, AppendOutcome::Dropped);
    assert_eq!(writer.line_watermark(), watermark_before + 2);
    // The raw stream keeps draining too — and in this tampered shape it
    // cannot get a segment either, so its batch is dropped as its *own*
    // stream-scoped gap (never merged with the normalized one).
    let raw_dropped = writer
        .append_raw(&vec![b'r'; FLUSH_MAX_BYTES])
        .await
        .unwrap();
    assert_eq!(raw_dropped.outcome, AppendOutcome::Dropped);
    assert_eq!(writer.raw_watermark(), FLUSH_MAX_BYTES as u64);

    // The normalized drops coalesce into exactly one record covering both
    // dropped lines; the committed prefix is unchanged and byte-exact;
    // degraded and refuse_new_start are latched.
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert!(snapshot.refuse_new_start);
    assert_eq!(snapshot.line_watermark, watermark_before + 2);
    assert_eq!(snapshot.gaps.len(), 2, "{:?}", snapshot.gaps);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Normalized);
    assert_eq!(snapshot.gaps[0].start, watermark_before + 1);
    assert_eq!(snapshot.gaps[0].end, watermark_before + 3);
    assert_eq!(snapshot.gaps[0].reason, GapReason::Missing);
    assert_eq!(snapshot.gaps[1].stream, LogStream::Raw);
    assert_eq!(snapshot.gaps[1].start, 0);
    assert_eq!(snapshot.gaps[1].end, FLUSH_MAX_BYTES as u64);
    assert_eq!(snapshot.gaps[1].reason, GapReason::Missing);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        committed_prefix
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        Vec::<u8>::new()
    );

    // Only future terminal starts are refused, and the latch is durable
    // across a fresh open. The draining writer is shut down first: while
    // it is attached, the exclusive lease is the primary refusal (a
    // second writer is refused for ownership before any state decision).
    drop(writer);
    assert!(matches!(
        store.open_writer(&log).await,
        Err(StorageError::RefuseNewStart { .. })
    ));
    let store = LogStore::open(&root).await.unwrap();
    assert!(
        store
            .recovery_snapshot(&log)
            .await
            .unwrap()
            .refuse_new_start
    );
    assert!(matches!(
        store.open_writer(&log).await,
        Err(StorageError::RefuseNewStart { .. })
    ));
}

/// Rebuild-without-prior-recover regression: after a destructive database
/// rebuild, an unrecovered old-epoch `seg-000001.log` occupies the name the
/// fresh database is about to mint. The writer must refuse with a typed
/// `RecoveryRequired` instead of appending a new header/frames onto the old
/// file and adopting its bytes, and reads must never expose the old
/// committed prefix. Only after recovery quarantined the orphans does the
/// new epoch write.
#[tokio::test]
async fn rebuild_without_prior_recover_refuses_instead_of_adopting_old_files() {
    let root = TempRoot::new("rebuild-no-recover");
    let old = log_identity(EPOCH_A);
    let store = seeded(&root, &old).await;
    drop(store);
    let old_first = std::fs::read(seg_file(&root, 1)).unwrap();

    // Destructive rebuild: delete the database with its WAL sidecars.
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(root.db().with_file_name(format!("terminal.db{suffix}")));
    }

    let store = LogStore::open(&root).await.unwrap();
    let rebuilt = log_identity(EPOCH_B);
    let mut writer = store.open_writer(&rebuilt).await.unwrap();
    writer
        .append_line(1, "rebuilt")
        .await
        .expect("the append itself only buffers");
    match writer.flush().await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("already exists"), "{detail}");
            assert!(detail.contains("recovery"), "{detail}");
        }
        other => panic!(
            "expected RecoveryRequired on the orphan name collision, got {:?}",
            other.map_err(|error| error.to_string())
        ),
    }
    // The old file was never appended to or adopted: byte-identical, and
    // the verification read refuses rather than exposing its prefix.
    assert_eq!(
        std::fs::read(seg_file(&root, 1)).unwrap(),
        old_first,
        "the orphan file must not have received a new header or frames"
    );
    match store.read_committed(&rebuilt, LogStream::Normalized).await {
        Err(StorageError::RecoveryRequired { .. }) => {}
        other => panic!(
            "read must refuse the pending row over the foreign file, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    // Recovery holds the same exclusive lease: the writer must be shut
    // down first (its poisoned normalized stream makes `close` report the
    // refusal, which is expected and ignored here).
    let _ = writer.close().await;

    // Recovery quarantines the zero-committed pending row and its foreign
    // file plus every unclaimed old-epoch file; writing then succeeds.
    let report = store.recover(&rebuilt).await.unwrap();
    let mut tombstones = Vec::new();
    let mut orphans = Vec::new();
    for action in &report.actions {
        match action {
            RecoveryAction::ZeroByteRowTombstoned { segment_id } => tombstones.push(*segment_id),
            RecoveryAction::OrphanSegmentQuarantined { file_name } => {
                orphans.push(file_name.clone())
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
    assert_eq!(tombstones.len(), 1);
    assert_eq!(
        orphans,
        vec!["seg-000002.log".to_owned(), "seg-000003.log".to_owned(),],
        "{:?}",
        report.actions
    );
    assert_idempotent(&store, &rebuilt).await;

    let mut writer = store.open_writer(&rebuilt).await.unwrap();
    writer.append_line(1, "rebuilt").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_eq!(
        store
            .read_committed(&rebuilt, LogStream::Normalized)
            .await
            .unwrap(),
        b"rebuilt".to_vec()
    );
    // The old epoch's bytes survive only as quarantine artifacts.
    assert_eq!(
        std::fs::read(root.join("quarantine-seg-000001.log")).unwrap(),
        old_first
    );
}

/// A checksummed, cleanly-scanned file whose coordinates disagree with the
/// row's indexed range fabricates continuity: recovery must lose the
/// segment as corrupt (explicit gap + degraded), and reads must refuse
/// before recovery ran.
#[tokio::test]
async fn row_range_mismatch_with_clean_file_is_an_explicit_gap() {
    let root = TempRoot::new("range-mismatch");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);

    // Claim one line more than the file holds (lines 1-2 -> [1,4)); the
    // file itself stays byte-identical and cleanly scannable.
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE segment SET last_line = 4 WHERE segment_id = 1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.read_committed(&log, LogStream::Normalized).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("indexed range"), "{detail}");
        }
        other => panic!(
            "read must refuse the mismatch, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Normalized,
            range_start: 1,
            range_end: 4,
            reason: GapReason::Corrupt,
            file_name: "seg-000001.log".into(),
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.line_watermark, 4);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].start, 1);
    assert_eq!(snapshot.gaps[0].end, 4);
    // The surviving segments read back; the raw stream is unaffected.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"threefour".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk".to_vec()
    );
    assert_idempotent(&store, &log).await;
}

/// The 1024 persisted gap-record cap at its boundary: 1025 disjoint losses
/// of one stream through the recovery seam coalesce conservatively into
/// exactly 1024 persisted records — every true loss stays covered (no
/// narrowing), the terminal degrades, and the other stream records
/// nothing.
#[tokio::test]
async fn gap_records_conservatively_coarsen_at_the_1024_cap() {
    let root = TempRoot::new("gap-cap");
    let log = log_identity(EPOCH_A);
    // Only a terminal row is needed (no live segments): open a writer and
    // drop it again without appending.
    let store = LogStore::open(&root).await.unwrap();
    drop(store.open_writer(&log).await.unwrap());

    // 1025 disjoint normalized ranges, each 2 lines wide with 8-line holes,
    // whose files do not exist: every row is lost as `missing`.
    const GAPS: usize = MAX_GAP_RECORDS + 1;
    let pool = raw_pool(&root).await;
    let mut tx = pool.begin().await.unwrap();
    for i in 0..GAPS {
        let id = 900_000 + i as i64 + 1;
        let start = 1 + i * 10;
        let end = start + 2;
        sqlx::query(
            "INSERT INTO segment
                 (segment_id, session_source, external_session_id, terminal_id, kind,
                  file_name, first_line, last_line, first_offset, last_offset,
                  committed_bytes, fsynced_bytes, state, created_ms)
             VALUES (?1, 'pi', 'session-1', ?2, 'normalized', ?3, ?4, ?5, 0, 0,
                     500, 500, 'sealed', 0)",
        )
        .bind(id)
        .bind(TERMINAL)
        .bind(format!("seg-{id:06}.log"))
        .bind(start as i64)
        .bind(end as i64)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions.len(),
        GAPS,
        "every lost segment is an explicit action"
    );
    assert!(
        report
            .actions
            .iter()
            .all(|action| matches!(action, RecoveryAction::SegmentLost { .. }))
    );

    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(
        snapshot.gaps.len(),
        MAX_GAP_RECORDS,
        "the persisted set must sit exactly at the cap"
    );
    assert!(
        snapshot
            .gaps
            .iter()
            .all(|gap| gap.stream == LogStream::Normalized)
    );
    // No narrowing and no overlap: sorted, disjoint, and every original
    // loss stays inside some record (coarsening only ever widens).
    let mut records = snapshot.gaps.clone();
    records.sort_by_key(|gap| (gap.start, gap.end));
    assert_eq!(records[0].start, 1);
    for pair in records.windows(2) {
        assert!(pair[0].end < pair[1].start, "records must stay disjoint");
    }
    let mut total = 0u64;
    for gap in &records {
        total += gap.end - gap.start;
    }
    // 1025 losses of 2 lines each; one coarsening merge swallowed exactly
    // the smallest hole (8 lines): the union widened by it, never narrowed.
    assert_eq!(total, (GAPS as u64) * 2 + 8, "conservative union preserved");
    for i in 0..GAPS as u64 {
        let start = 1 + i * 10;
        let covered = records
            .iter()
            .any(|gap| gap.start <= start && start + 2 <= gap.end);
        assert!(
            covered,
            "original loss [{},{}) narrowed away",
            start,
            start + 2
        );
    }
    // Raw isolation: the other stream records nothing and stays readable.
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        Vec::<u8>::new()
    );
    assert_idempotent(&store, &log).await;
}

/// The recovery pass holds the log's exclusive writer lease: it refuses
/// (typed) while a writer is attached, and runs once the writer closed.
#[tokio::test]
async fn recovery_refuses_while_a_writer_is_attached() {
    let root = TempRoot::new("recovery-lease");
    let log = log_identity(EPOCH_A);
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();
    assert!(matches!(
        store.recover(&log).await,
        Err(StorageError::WriterAlreadyActive { .. })
    ));
    // Graceful close releases the lease deterministically; the pass then
    // runs as a zero-action idempotent pass over the healthy log.
    writer.close().await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert!(report.is_empty());
    assert!(!report.degraded);
    assert!(report.gaps.is_empty());
    assert_idempotent(&store, &log).await;
}

/// The public recovery report is domain-level only: repaired/degraded and
/// the explicit gap state (stream, domain range, reason) — never segment
/// ids, file names, quarantine artifacts, or truncation boundaries.
#[tokio::test]
async fn recovery_report_is_domain_level() {
    let root = TempRoot::new("report-surface");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    std::fs::remove_file(seg_file(&root, 1)).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert!(report.repaired);
    assert!(report.degraded);
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].stream, LogStream::Normalized);
    assert_eq!(report.gaps[0].start, 1);
    assert_eq!(report.gaps[0].end, 3);
    assert_eq!(report.gaps[0].reason, GapReason::Missing);
    // A rerun reports nothing repaired over an equal durable state.
    let rerun = store.recover(&log).await.unwrap();
    assert!(!rerun.repaired);
    assert!(rerun.degraded, "the degraded latch is permanent");
    assert_eq!(rerun.gaps, report.gaps);
}

/// An uncovered hole in the ordered chain — a segment row deleted
/// directly, leaving a range the watermark consumed with no segment and
/// no gap — is an unrecorded loss: reads refuse until recovery recorded
/// the hole as an explicit gap and latched `degraded`.
#[tokio::test]
async fn uncovered_chain_hole_is_gapped_by_recovery_and_refused_by_reads() {
    let root = TempRoot::new("chain-hole");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // Delete the middle normalized row's index entry directly (its file
    // becomes an orphan): lines 3-4 are consumed but unbacked.
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE terminal SET active_normalized_segment = NULL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM segment WHERE segment_id = 2")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.read_committed(&log, LogStream::Normalized).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("hole [3,5)"), "{detail}");
        }
        other => panic!(
            "read must refuse the uncovered hole, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-000002.log".into()
            },
            RecoveryAction::UncoveredHoleGapped {
                stream: LogStream::Normalized,
                range_start: 3,
                range_end: 5,
            },
        ]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.line_watermark, 4);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].start, 3);
    assert_eq!(snapshot.gaps[0].end, 5);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_idempotent(&store, &log).await;
}

/// Ranges that violate the ordered chain fabricate continuity even when
/// every row validates alone against its own file — here the retained
/// floor sits above the first row's start. Reads refuse at the chain
/// check, and recovery loses the violating row as corrupt (explicit gap,
/// degraded, file quarantined whole).
#[tokio::test]
async fn chain_ranges_below_the_retained_floor_are_lost_as_corrupt() {
    let root = TempRoot::new("chain-overlap");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // Both rows and files stay consistent; only the floor moves above the
    // first row's start (a range expiry can never legitimately do that).
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE terminal SET retained_first_line = 3")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.read_committed(&log, LogStream::Normalized).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("overlap"), "{detail}");
        }
        other => panic!(
            "read must refuse the chain violation, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Normalized,
            range_start: 1,
            range_end: 3,
            reason: GapReason::Corrupt,
            file_name: "seg-000001.log".into(),
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].start, 1);
    assert_eq!(snapshot.gaps[0].end, 3);
    assert!(root.join("quarantine-seg-000001.log").is_file());
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"threefour".to_vec()
    );
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_idempotent(&store, &log).await;
}

/// Segment rows whose indexed ranges extend past the watermark-derived
/// expected end (the watermark was rolled back by direct tampering while
/// every row still validates alone against its file): the chain is
/// validated *against* the watermark, so reads refuse before recovery,
/// recovery loses each offending row as corrupt — never adopting its
/// range as continuity beyond the durable watermark — and the surviving
/// explicit gap keeps the (never-decreasing) watermark's numbering
/// consistent. Both streams are covered; the other stream is untouched.
#[tokio::test]
async fn rows_beyond_the_watermark_are_refused_then_lost_as_corrupt() {
    for stream in [LogStream::Normalized, LogStream::Raw] {
        let root = TempRoot::new("beyond-watermark");
        let log = log_identity(EPOCH_A);
        let store = seeded(&root, &log).await;
        drop(store);
        // Roll the stream's watermark back below its committed ranges:
        // every row of the stream now ends past the expected end
        // (normalized: watermark+1; raw: watermark).
        let pool = raw_pool(&root).await;
        let statement = match stream {
            LogStream::Normalized => "UPDATE terminal SET line_watermark = 1",
            LogStream::Raw => "UPDATE terminal SET raw_watermark = 2",
        };
        sqlx::query(statement).execute(&pool).await.unwrap();
        pool.close().await;

        let store = LogStore::open(&root).await.unwrap();
        // Read-before-recovery: the verification read refuses the
        // out-of-watermark chain instead of returning it.
        match store.read_committed(&log, stream).await {
            Err(StorageError::RecoveryRequired { detail }) => {
                assert!(detail.contains("past the watermark"), "{detail}");
            }
            other => panic!(
                "{stream:?}: read must refuse the beyond-watermark chain, got {:?}",
                other.map(|v| v.len()).map_err(|error| error.to_string())
            ),
        }
        // The other stream of the same terminal stays fully readable.
        let (other, other_bytes) = match stream {
            LogStream::Normalized => (LogStream::Raw, &b"raw-chunk"[..]),
            LogStream::Raw => (LogStream::Normalized, &b"onetwothreefour"[..]),
        };
        assert_eq!(
            store.read_committed(&log, other).await.unwrap(),
            other_bytes.to_vec(),
            "{stream:?}: the other stream must stay readable"
        );

        let report = store.recover(&log).await.unwrap();
        // Every row of the tampered stream is lost as corrupt, one repair
        // per convergence iteration (the seeded log holds two normalized
        // segments, one raw segment).
        let expected_lost = match stream {
            LogStream::Normalized => 2,
            LogStream::Raw => 1,
        };
        let lost = report
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    RecoveryAction::SegmentLost {
                        stream: lost_stream,
                        reason: GapReason::Corrupt,
                        ..
                    } if *lost_stream == stream
                )
            })
            .count();
        assert_eq!(lost, expected_lost, "{stream:?}: {:?}", report.actions);
        assert!(
            report
                .actions
                .iter()
                .all(|action| !matches!(action, RecoveryAction::UncoveredHoleGapped { .. })),
            "{stream:?}: the coalesced loss must cover the watermark's range, \
             not fabricate a second hole: {:?}",
            report.actions
        );

        let snapshot = store.recovery_snapshot(&log).await.unwrap();
        assert!(snapshot.degraded, "{stream:?}");
        assert_eq!(snapshot.gaps.len(), 1, "{stream:?}: {:?}", snapshot.gaps);
        let gap = snapshot.gaps[0].clone();
        assert_eq!(gap.stream, stream);
        assert_eq!(gap.reason, GapReason::Corrupt);
        match stream {
            LogStream::Normalized => {
                // Watermark never decreased: the rolled-back value stands,
                // and the whole former chain [1,5) is one explicit gap.
                assert_eq!(snapshot.line_watermark, 1);
                assert_eq!(gap.start, 1);
                assert_eq!(gap.end, 5);
            }
            LogStream::Raw => {
                assert_eq!(snapshot.raw_watermark, 2);
                assert_eq!(gap.start, 0);
                assert_eq!(gap.end, 9);
            }
        }
        // The read now succeeds over the recovered state (everything the
        // chain claimed was lost) and the other stream is byte-exact.
        assert_eq!(
            store.read_committed(&log, stream).await.unwrap(),
            Vec::<u8>::new(),
            "{stream:?}"
        );
        assert_eq!(
            store.read_committed(&log, other).await.unwrap(),
            other_bytes.to_vec(),
            "{stream:?}"
        );
        // Numbering continues past the rolled-back watermark without
        // reuse.
        let mut writer = store.open_writer(&log).await.unwrap();
        match stream {
            LogStream::Normalized => {
                assert_eq!(writer.line_watermark(), 1);
                writer.append_line(2, "two").await.unwrap();
            }
            LogStream::Raw => {
                assert_eq!(writer.raw_watermark(), 2);
                writer.append_raw(b"next").await.unwrap();
            }
        }
        writer.flush().await.unwrap();
        writer.close().await.unwrap();
        assert_idempotent(&store, &log).await;
    }
}

/// A retained floor beyond the watermark-derived expected end is broken
/// bookkeeping: reads refuse until recovery ran, recovery clamps the
/// floor back under the expected end, and no committed data is lost,
/// quarantined, or degraded.
#[tokio::test]
async fn retained_floor_beyond_the_watermark_is_clamped_by_recovery() {
    let root = TempRoot::new("floor-beyond");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    // First bring the normalized stream into a consistent gap-covered
    // state (both segments lost), then push its floor past every consumed
    // position: the floor now claims retention gave up lines the
    // watermark never consumed.
    std::fs::remove_file(seg_file(&root, 1)).unwrap();
    std::fs::remove_file(seg_file(&root, 2)).unwrap();
    let store = LogStore::open(&root).await.unwrap();
    let first = store.recover(&log).await.unwrap();
    assert_eq!(first.actions.len(), 2);

    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE terminal SET retained_first_line = 99")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.read_committed(&log, LogStream::Normalized).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("retained floor"), "{detail}");
        }
        other => panic!(
            "read must refuse the floor beyond the watermark, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::RetainedFloorClamped {
            stream: LogStream::Normalized
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    // The floor is back under the expected end (watermark 4 + 1); the
    // earlier losses stay exactly as recorded, and nothing degraded
    // further (the latch itself is permanent from the losses).
    assert_eq!(snapshot.retained_first_line, 5);
    assert_eq!(snapshot.line_watermark, 4);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].start, 1);
    assert_eq!(snapshot.gaps[0].end, 5);
    assert!(snapshot.degraded);
    // The raw stream never noticed, and the normalized read now succeeds
    // over the gap-covered state.
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk".to_vec()
    );
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        Vec::<u8>::new()
    );
    assert_idempotent(&store, &log).await;
}

/// A raw-stream chain hole is gapped in byte coordinates only; the
/// normalized stream of the same terminal stays fully readable.
#[tokio::test]
async fn raw_chain_hole_is_stream_scoped() {
    let root = TempRoot::new("chain-raw-hole");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);
    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE terminal SET active_raw_segment = NULL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM segment WHERE segment_id = 3")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let store = LogStore::open(&root).await.unwrap();
    match store.read_committed(&log, LogStream::Raw).await {
        Err(StorageError::RecoveryRequired { detail }) => {
            assert!(detail.contains("hole [0,9)"), "{detail}");
        }
        other => panic!(
            "raw read must refuse the hole, got {:?}",
            other.map(|v| v.len()).map_err(|error| error.to_string())
        ),
    }
    // The normalized stream is untouched by the raw chain's hole.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwothreefour".to_vec()
    );
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![
            RecoveryAction::OrphanSegmentQuarantined {
                file_name: "seg-000003.log".into()
            },
            RecoveryAction::UncoveredHoleGapped {
                stream: LogStream::Raw,
                range_start: 0,
                range_end: 9,
            },
        ]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Raw);
    assert_eq!(snapshot.raw_watermark, 9);
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_raw(b"next").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_idempotent(&store, &log).await;
}

/// IEEE CRC32 (the frame header's checksum), table-less, so the test can
/// tamper *and re-checksum* a committed frame — the shape of an on-disk
/// corruption that fixes its own checksum instead of tripping it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn crc32_helper_matches_ieee() {
    // The standard check value of the IEEE CRC-32 polynomial.
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

/// Clear the line-end flag of the final frame of a committed normalized
/// segment and fix the frame header's checksum, then prove every surface
/// that validates a committed prefix refuses it: the verification read
/// (never returning a partial line as committed), the writer reattach,
/// and recovery — which loses the segment as corrupt and converges. The
/// raw stream is unchanged: its frames carry no line flags, and it reads
/// through the whole scenario untouched.
#[tokio::test]
async fn cleared_line_end_flag_on_a_committed_prefix_is_rejected_everywhere() {
    const SEGMENT_HEADER_LEN: usize = 64;
    const FRAME_HEADER_LEN: usize = 40;
    let root = TempRoot::new("cleared-flag");
    let log = log_identity(EPOCH_A);
    let store = seeded(&root, &log).await;
    drop(store);

    // Segment 2 is the active normalized segment (lines 3-4); its final
    // frame is line 4's single chunk, carrying the line-end flag.
    let path = seg_file(&root, 2);
    let mut data = std::fs::read(&path).unwrap();
    let committed = data.len();
    let mut off = SEGMENT_HEADER_LEN;
    let mut last = None;
    while off + FRAME_HEADER_LEN <= data.len() {
        let payload_len = u32::from_le_bytes(data[off + 32..off + 36].try_into().unwrap()) as usize;
        let end = off + FRAME_HEADER_LEN + payload_len + 4;
        assert!(end <= data.len(), "frame runs past the committed boundary");
        last = Some(off);
        off = end;
    }
    let last = last.expect("the segment holds committed frames");
    assert_eq!(off, committed, "the scan must end exactly at the boundary");
    assert_eq!(data[last + 7], 0x01, "the final frame carries the flag");
    data[last + 7] = 0x00; // clear FRAME_FLAG_LINE_END
    let fixed_crc = crc32(&data[last..last + 36]);
    data[last + 36..last + 40].copy_from_slice(&fixed_crc.to_le_bytes());
    std::fs::write(&path, &data).unwrap();

    let store = LogStore::open(&root).await.unwrap();
    // Read surface: refuse — a mid-line final frame must never be returned
    // as the committed range's complete last line.
    assert!(matches!(
        store.read_committed(&log, LogStream::Normalized).await,
        Err(StorageError::RecoveryRequired { .. })
    ));
    // Raw surface: unchanged and fully readable.
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk".to_vec()
    );
    // Reattach surface: refuse to append behind the tampered prefix.
    assert!(matches!(
        store.open_writer(&log).await,
        Err(StorageError::RecoveryRequired { .. })
    ));

    // Recovery: the segment is row-inconsistent, so it is lost whole as
    // corrupt — explicit gap over its range, degraded latch, watermark
    // preserved — and the pass converges.
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::SegmentLost {
            stream: LogStream::Normalized,
            range_start: 3,
            range_end: 5,
            reason: GapReason::Corrupt,
            file_name: "seg-000002.log".into(),
        }]
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.line_watermark, 4);
    assert_eq!(snapshot.gaps.len(), 1);
    assert_eq!(snapshot.gaps[0].stream, LogStream::Normalized);
    assert_eq!(snapshot.gaps[0].start, 3);
    assert_eq!(snapshot.gaps[0].end, 5);
    assert_eq!(snapshot.gaps[0].reason, GapReason::Corrupt);
    // Post-recovery reads: the surviving prefix plus nothing from the
    // tampered segment; the raw stream still reads through.
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwo".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk".to_vec()
    );
    // Numbering continues past the gapped range without reuse.
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.flush().await.unwrap();
    writer.close().await.unwrap();
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"onetwofive".to_vec()
    );
    assert_idempotent(&store, &log).await;
}
