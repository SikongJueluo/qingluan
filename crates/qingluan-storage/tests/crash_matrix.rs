//! Abrupt-crash matrix for the S2 durability sequence, with real
//! subprocess deaths (`libc::_exit(70)`) at exact failpoints.
//!
//! `harness = false`: this binary doubles as the crash child. Invoked
//! with `--crash-child <root> <point> <stream>` it drives the writer and
//! dies hard at the named [`CrashPoint`]; with `--recover-crash <root>`
//! it dies hard after the quarantine artifact of a tail repair is synced
//! but before the live file is truncated. In its test mode it spawns
//! those children, then performs the recovery *in this (fresh) process*,
//! runs it a second time (zero actions, equal durable snapshot), and
//! verifies that writing continues with unreused numbering and the
//! committed prefix reads back byte-exact. Power-loss variants
//! physically truncate the crashed file after the child died. Every
//! child is waited on and every root is a temp dir removed on exit: no
//! child, file, or directory residue.

use std::path::{Path, PathBuf};
use std::process::Command;

use qingluan_core::terminal::{
    ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TerminalId, TerminalRef,
};
use qingluan_storage::{CrashPoint, CrashSink, GapReason, LogStore, LogStream, RecoveryAction};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s2b-crash-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

fn parse_point(name: &str) -> Option<CrashPoint> {
    Some(match name {
        "segment-before" => CrashPoint::SegmentBefore,
        "segment-row-inserted" => CrashPoint::SegmentRowInserted,
        "segment-header-written" => CrashPoint::SegmentHeaderWritten,
        "segment-header-synced" => CrashPoint::SegmentHeaderSynced,
        "frame-before-write" => CrashPoint::FrameBeforeWrite,
        "frame-after-write" => CrashPoint::FrameAfterWrite,
        "frame-after-sync" => CrashPoint::FrameAfterSync,
        "txn-begin" => CrashPoint::TxnBegin,
        "txn-update" => CrashPoint::TxnUpdate,
        "txn-before-commit" => CrashPoint::TxnBeforeCommit,
        "txn-after-commit" => CrashPoint::TxnAfterCommit,
        "publish-before" => CrashPoint::PublishBefore,
        "publish-after" => CrashPoint::PublishAfter,
        _ => return None,
    })
}

/// The child: commit batch one, optionally seal the stream (so batch two
/// creates a fresh segment and walks every creation failpoint), install a
/// sink that dies hard at the target, then append batch two and flush it
/// (the failpoints live on the flushed commit sequence).
async fn crash_child(root: &Path, point: &str, raw: bool, seal: bool) -> Result<(), String> {
    let target = parse_point(point).ok_or_else(|| format!("unknown point {point}"))?;
    let store = LogStore::open(root)
        .await
        .map_err(|error| format!("open: {error}"))?;
    let log = log_identity();
    let mut writer = store
        .open_writer(&log)
        .await
        .map_err(|error| format!("attach: {error}"))?;
    // Batch one commits cleanly first, so every crashpoint below fires
    // during batch two's creation/append/commit on a fresh segment.
    if raw {
        writer
            .append_raw(b"one")
            .await
            .map_err(|error| format!("append one: {error}"))?;
        writer
            .flush()
            .await
            .map_err(|error| format!("flush one: {error}"))?;
        if seal {
            writer
                .seal(LogStream::Raw)
                .await
                .map_err(|error| format!("seal: {error}"))?;
        }
    } else {
        writer
            .append_line(1, "one")
            .await
            .map_err(|error| format!("append 1: {error}"))?;
        writer
            .flush()
            .await
            .map_err(|error| format!("flush one: {error}"))?;
        if seal {
            writer
                .seal(LogStream::Normalized)
                .await
                .map_err(|error| format!("seal: {error}"))?;
        }
    }
    let sink: CrashSink = std::sync::Arc::new(move |hit: CrashPoint| {
        if hit == target {
            // Real abrupt death: no unwinding, no at-exit handlers, no
            // buffer flushes — the writer's file descriptors and the
            // uncommitted transaction are simply gone.
            unsafe { libc::_exit(70) };
        }
    });
    writer.set_crash_sink(Some(sink));
    if raw {
        writer
            .append_raw(b"two")
            .await
            .map_err(|error| format!("append two: {error}"))?;
        writer
            .flush()
            .await
            .map_err(|error| format!("flush two: {error}"))?;
    } else {
        writer
            .append_line(2, "two")
            .await
            .map_err(|error| format!("append 2: {error}"))?;
        writer
            .flush()
            .await
            .map_err(|error| format!("flush two: {error}"))?;
    }
    Ok(())
}

/// The loss-crash child: two committed normalized segments plus a raw
/// chunk, the first segment's payload corrupted, then recovery dying hard
/// exactly at `RecoveryRowsCommitted` — after the single loss transaction
/// committed (gap + pointer clear + tombstone + degraded latch), before
/// the file was renamed to quarantine.
async fn loss_crash_child(root: &Path) -> Result<(), String> {
    let store = LogStore::open(root)
        .await
        .map_err(|error| format!("open: {error}"))?;
    let log = log_identity();
    let mut writer = store
        .open_writer(&log)
        .await
        .map_err(|error| format!("attach: {error}"))?;
    for (line, text) in [(1u64, "one"), (2, "two"), (3, "three"), (4, "four")] {
        writer
            .append_line(line, text)
            .await
            .map_err(|error| format!("append {line}: {error}"))?;
        if line == 2 {
            writer
                .seal(LogStream::Normalized)
                .await
                .map_err(|error| format!("seal: {error}"))?;
        }
    }
    writer
        .append_raw(b"raw-chunk")
        .await
        .map_err(|error| format!("append raw: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush: {error}"))?;
    writer
        .close()
        .await
        .map_err(|error| format!("close: {error}"))?;
    // Corrupt the first segment's last committed byte.
    let segment = root.join("seg-000001.log");
    let mut data = std::fs::read(&segment).map_err(|e| format!("read: {e}"))?;
    let last = data.len() - 8;
    data[last] ^= 0xFF;
    std::fs::write(&segment, data).map_err(|e| format!("write: {e}"))?;
    let sink: CrashSink = std::sync::Arc::new(|hit: CrashPoint| {
        if hit == CrashPoint::RecoveryRowsCommitted {
            // Real abrupt death: no unwinding, no at-exit handlers.
            unsafe { libc::_exit(70) };
        }
    });
    store.set_crash_sink(Some(sink));
    store
        .recover(&log)
        .await
        .map_err(|error| format!("recover: {error}"))?;
    Ok(())
}

/// The writer-hold child: opens the exclusive writer, appends to both
/// streams, holds the lease for `hold_ms`, then exits cleanly (the
/// kernel releases the lease at process exit).
async fn hold_writer_child(root: &Path, hold_ms: u64) -> Result<(), String> {
    let store = LogStore::open(root)
        .await
        .map_err(|error| format!("open: {error}"))?;
    let log = log_identity();
    let mut writer = store
        .open_writer(&log)
        .await
        .map_err(|error| format!("attach: {error}"))?;
    writer
        .append_line(1, "one")
        .await
        .map_err(|error| format!("append: {error}"))?;
    writer
        .append_raw(b"raw-one")
        .await
        .map_err(|error| format!("append raw: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush: {error}"))?;
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    writer
        .close()
        .await
        .map_err(|error| format!("close: {error}"))?;
    Ok(())
}

/// The recovery-crash child: a committed line plus a simulated uncommitted
/// tail, then recovery dying hard right after the quarantine artifact
/// became durable (before the live truncation).
async fn recover_crash_child(root: &Path) -> Result<(), String> {
    let store = LogStore::open(root)
        .await
        .map_err(|error| format!("open: {error}"))?;
    let log = log_identity();
    let mut writer = store
        .open_writer(&log)
        .await
        .map_err(|error| format!("attach: {error}"))?;
    writer
        .append_line(1, "one")
        .await
        .map_err(|error| format!("append 1: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush 1: {error}"))?;
    drop(writer);
    let segment = root.join("seg-000001.log");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .map_err(|error| format!("tail append: {error}"))?;
    std::io::Write::write_all(&mut file, b"junk-tail").map_err(|e| format!("tail: {e}"))?;
    drop(file);
    let sink: CrashSink = std::sync::Arc::new(|hit: CrashPoint| {
        if hit == CrashPoint::RecoveryArtifactSynced {
            unsafe { libc::_exit(70) };
        }
    });
    store.set_crash_sink(Some(sink));
    store
        .recover(&log)
        .await
        .map_err(|error| format!("recover: {error}"))?;
    Ok(())
}

fn spawn_child(args: &[&str]) {
    let exe = std::env::current_exe().expect("test binary path");
    let status = Command::new(exe)
        .args(args)
        .status()
        .expect("spawn crash child");
    assert_eq!(
        status.code(),
        Some(70),
        "child {args:?} must die abruptly with _exit(70), got {status:?}"
    );
}

/// Fresh-process recovery after the abrupt death, twice: the second pass
/// takes zero actions over an equal durable state. Returns the first
/// report.
async fn recover_twice(root: &Path) -> qingluan_storage::RecoveryReport {
    let log = log_identity();
    let store = LogStore::open(root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let rerun = store.recover(&log).await.unwrap();
    assert!(rerun.actions.is_empty(), "second recovery: {rerun:?}");
    assert_eq!(
        store.recovery_snapshot(&log).await.unwrap(),
        snapshot,
        "durable state changed on the second recovery"
    );
    report
}

async fn raw_pool(root: &Path) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.join("terminal.db"));
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// The file of the still-empty segment row of the second batch (test-side
/// inspection before recovery).
async fn empty_segment_file(root: &Path) -> PathBuf {
    let pool = raw_pool(root).await;
    let file_name: String =
        sqlx::query_scalar("SELECT file_name FROM segment WHERE committed_bytes = 0 LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    root.join(file_name)
}

fn truncate_file(path: &Path, len: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(len).unwrap();
}

/// What the normalized matrix expects per failpoint: the classification
/// of the crash residue, the watermark that must survive, and the
/// committed bytes that must read back.
struct Case {
    classification: Classification,
    watermark: u64,
    committed: &'static [u8],
}

enum Classification {
    /// The crash left nothing behind: zero recovery actions.
    Clean,
    /// The creation transaction survived without its file: tombstone.
    Tombstone,
    /// Durable frames beyond the committed boundary: tail quarantine.
    Tail,
}

async fn run_case(name: &'static str, raw: bool, seal: bool, case: Case) {
    let root = TempRoot::new(name);
    let stream = if raw { "raw" } else { "norm" };
    let mode = if seal { "seal" } else { "instream" };
    spawn_child(&["--crash-child", root.to_str().unwrap(), name, stream, mode]);
    let log = log_identity();
    let report = recover_twice(&root).await;

    match case.classification {
        Classification::Clean => assert!(
            report.actions.is_empty(),
            "{name}: expected zero actions, got {:?}",
            report.actions
        ),
        Classification::Tombstone => {
            assert_eq!(report.actions.len(), 1, "{name}: {:?}", report.actions);
            assert!(
                matches!(
                    report.actions[0],
                    RecoveryAction::ZeroByteRowTombstoned { .. }
                ),
                "{name}: {:?}",
                report.actions[0]
            );
        }
        Classification::Tail => {
            assert_eq!(report.actions.len(), 1, "{name}: {:?}", report.actions);
            assert!(
                matches!(report.actions[0], RecoveryAction::TailQuarantined { .. }),
                "{name}: {:?}",
                report.actions[0]
            );
        }
    }

    // Writing continues with unreused numbering and the committed prefix
    // reads back byte-exact.
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    let stream_of = if raw {
        LogStream::Raw
    } else {
        LogStream::Normalized
    };
    if raw {
        assert_eq!(writer.raw_watermark(), case.watermark, "{name}");
        writer.append_raw(b"three").await.unwrap();
        writer.flush().await.unwrap();
    } else {
        assert_eq!(writer.line_watermark(), case.watermark, "{name}");
        writer
            .append_line(case.watermark + 1, "three")
            .await
            .unwrap();
        writer.flush().await.unwrap();
    }
    drop(writer);
    let mut expected = case.committed.to_vec();
    expected.extend_from_slice(b"three");
    assert_eq!(
        store.read_committed(&log, stream_of).await.unwrap(),
        expected,
        "{name}: committed prefix must read back byte-exact"
    );
}

/// Power-loss samples: after the abrupt death, physically truncate the
/// crashed file to simulate unsynced writes being gone.
async fn run_power_case(name: &'static str, point: &'static str, seal: bool, mode: &'static str) {
    let root = TempRoot::new(name);
    let child_mode = if seal { "seal" } else { "instream" };
    spawn_child(&[
        "--crash-child",
        root.to_str().unwrap(),
        point,
        "norm",
        child_mode,
    ]);
    let log = log_identity();
    match mode {
        // Crash mid-append of an unsynced creation: even the header is
        // gone, so the zero-committed row is tombstoned whole.
        "zero" => {
            let path = empty_segment_file(&root).await;
            truncate_file(&path, 0);
            let report = recover_twice(&root).await;
            assert_eq!(report.actions.len(), 1, "{name}: {:?}", report.actions);
            assert!(
                matches!(
                    report.actions[0],
                    RecoveryAction::ZeroByteRowTombstoned { .. }
                ),
                "{name}: {:?}",
                report.actions[0]
            );
        }
        // Synced frames torn mid-frame beyond the committed boundary of
        // the receiving segment: partial tail quarantined first.
        "torn" => {
            let pool = raw_pool(&root).await;
            let (file_name, committed): (String, i64) = sqlx::query_as(
                "SELECT file_name, committed_bytes FROM segment
                  WHERE committed_bytes > 0 ORDER BY segment_id DESC LIMIT 1",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            pool.close().await;
            // 30 bytes past the boundary: mid-frame, never a whole frame.
            truncate_file(&root.join(&file_name), (committed + 30) as u64);
            let report = recover_twice(&root).await;
            assert_eq!(
                report.actions,
                vec![RecoveryAction::TailQuarantined {
                    file_name,
                    quarantined_bytes: 30,
                    truncated_to: committed as u64,
                }],
                "{name}"
            );
        }
        // Committed data physically lost below the committed boundary:
        // explicit gap + degraded, watermark preserved.
        "committed" => {
            let pool = raw_pool(&root).await;
            let (file_name, committed): (String, i64) = sqlx::query_as(
                "SELECT file_name, committed_bytes FROM segment
                  WHERE committed_bytes > 0 ORDER BY segment_id DESC LIMIT 1",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            pool.close().await;
            truncate_file(&root.join(&file_name), (committed - 5) as u64);
            let report = recover_twice(&root).await;
            assert_eq!(
                report.actions,
                vec![RecoveryAction::SegmentLost {
                    stream: LogStream::Normalized,
                    range_start: 2,
                    range_end: 3,
                    reason: GapReason::Truncated,
                    file_name,
                }],
                "{name}"
            );
            let store = LogStore::open(&root).await.unwrap();
            let snapshot = store.recovery_snapshot(&log).await.unwrap();
            assert!(snapshot.degraded);
            assert_eq!(snapshot.line_watermark, 2, "watermark never decreases");
        }
        _ => unreachable!("unknown power mode"),
    }
    // The prefix survives in every variant and numbering continues.
    let store = LogStore::open(&root).await.unwrap();
    let mut writer = store.open_writer(&log).await.unwrap();
    let watermark = writer.line_watermark();
    writer.append_line(watermark + 1, "after").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"oneafter".to_vec(),
        "{name}"
    );
}

async fn recovery_crash_between_artifact_and_truncation() {
    let root = TempRoot::new("recovery-crash");
    spawn_child(&["--recover-crash", root.to_str().unwrap()]);
    let log = log_identity();
    // The child died with the artifact durable and the live file long: the
    // rerun converges (data never resurrects) and is idempotent.
    let store = LogStore::open(&root).await.unwrap();
    let report = store.recover(&log).await.unwrap();
    assert_eq!(
        report.actions,
        vec![RecoveryAction::TailQuarantined {
            file_name: "seg-000001.log".into(),
            quarantined_bytes: 9,
            truncated_to: 111,
        }]
    );
    let artifact = std::fs::read(root.join("quarantine-seg-000001.log")).unwrap();
    assert_eq!(artifact, b"junk-tail");
    assert_eq!(
        std::fs::metadata(root.join("seg-000001.log"))
            .unwrap()
            .len(),
        111
    );
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    let rerun = store.recover(&log).await.unwrap();
    assert!(rerun.is_empty());
    assert_eq!(store.recovery_snapshot(&log).await.unwrap(), snapshot);
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
}

/// A hard death exactly after the loss transaction committed: the durable
/// state must already hold the whole loss (gap + tombstone + degraded
/// latch; the active pointer cleared) — never half of it — and the
/// un-quarantined file converges as an orphan on the rerun, which is then
/// a zero-action idempotent pass.
async fn recovery_crash_inside_loss_commit() {
    let root = TempRoot::new("loss-crash");
    spawn_child(&["--loss-crash", root.to_str().unwrap()]);
    let log = log_identity();
    // Durable state at the crash boundary: the loss is fully recorded and
    // atomic (all four effects, or none).
    let store = LogStore::open(&root).await.unwrap();
    let at_crash = store.recovery_snapshot(&log).await.unwrap();
    assert!(at_crash.degraded, "the latch committed with the gap");
    assert_eq!(at_crash.gaps.len(), 1);
    assert_eq!(at_crash.gaps[0].stream, LogStream::Normalized);
    assert_eq!(at_crash.gaps[0].start, 1);
    assert_eq!(at_crash.gaps[0].end, 3);
    assert_eq!(at_crash.gaps[0].reason, GapReason::Corrupt);
    assert_eq!(
        at_crash
            .segments
            .iter()
            .filter(|row| row.kind == LogStream::Normalized)
            .count(),
        1,
        "the lost row is tombstoned in the same transaction"
    );
    assert_eq!(at_crash.line_watermark, 4, "the watermark never decreases");
    assert!(
        root.join("seg-000001.log").is_file(),
        "rename had not run yet"
    );

    // The rerun converges: the unclaimed file is quarantined as an orphan,
    // then a second pass is zero-action over an equal durable state.
    let report = recover_twice(&root).await;
    assert_eq!(
        report.actions,
        vec![RecoveryAction::OrphanSegmentQuarantined {
            file_name: "seg-000001.log".into()
        }],
        "the loss itself was already recorded before the crash"
    );
    assert!(root.join("quarantine-seg-000001.log").is_file());
    let snapshot = store.recovery_snapshot(&log).await.unwrap();
    assert!(snapshot.degraded);
    assert_eq!(snapshot.gaps, at_crash.gaps);

    // Writing continues with unreused numbering and byte-exact reads.
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(5, "five").await.unwrap();
    writer.append_raw(b"-more").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    assert_eq!(
        store
            .read_committed(&log, LogStream::Normalized)
            .await
            .unwrap(),
        b"threefourfive".to_vec()
    );
    assert_eq!(
        store.read_committed(&log, LogStream::Raw).await.unwrap(),
        b"raw-chunk-more".to_vec()
    );
}

/// One writer per log across processes: while the child holds the lease,
/// this process's attach (after its bounded wait) refuses with the typed
/// error; once the child exited, the lease is free (kernel-released) and
/// both streams continue the child's numbering.
async fn writer_lease_is_exclusive_across_processes() {
    let root = TempRoot::new("lease-process");
    let exe = std::env::current_exe().expect("test binary path");
    let mut child = Command::new(exe)
        .args([
            "--hold-writer",
            root.to_str().unwrap(),
            "2500".to_string().as_str(),
        ])
        .spawn()
        .expect("spawn hold-writer child");
    // Give the child time to attach (it sleeps well past our wait bound).
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity();
    match store.open_writer(&log).await {
        Err(qingluan_storage::StorageError::WriterAlreadyActive { detail }) => {
            assert!(detail.contains("exclusive writer lease"), "{detail}");
        }
        other => {
            panic!(
                "cross-process attach must refuse, got {:?}",
                other.map(|w| w.line_watermark())
            )
        }
    }
    let status = child.wait().expect("wait hold-writer child");
    assert_eq!(status.code(), Some(0));
    // The child's death released the lease: both streams continue.
    let mut writer = store.open_writer(&log).await.unwrap();
    assert_eq!(writer.line_watermark(), 1);
    assert_eq!(writer.raw_watermark(), 7);
    writer.append_line(2, "two").await.unwrap();
    writer.append_raw(b"raw-two").await.unwrap();
    writer.flush().await.unwrap();
    drop(writer);
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
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--crash-child") {
        let root = PathBuf::from(&args[2]);
        let point = args[3].clone();
        let raw = args.get(4).map(String::as_str) == Some("raw");
        let seal = args.get(5).map(String::as_str) == Some("seal");
        match crash_child(&root, &point, raw, seal).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("crash-child error: {error}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(String::as_str) == Some("--recover-crash") {
        let root = PathBuf::from(&args[2]);
        match recover_crash_child(&root).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("recover-crash error: {error}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(String::as_str) == Some("--loss-crash") {
        let root = PathBuf::from(&args[2]);
        match loss_crash_child(&root).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("loss-crash error: {error}");
                std::process::exit(2);
            }
        }
    }
    if args.get(1).map(String::as_str) == Some("--hold-writer") {
        let root = PathBuf::from(&args[2]);
        let hold_ms: u64 = args[3].parse().expect("hold ms");
        match hold_writer_child(&root, hold_ms).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("hold-writer error: {error}");
                std::process::exit(2);
            }
        }
    }

    let mut failures: Vec<&'static str> = Vec::new();
    let mut run =
        |name: &'static str, fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>| {
            print!("test {name} ... ");
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // The runtime is created by main; block on the case future
                // through a fresh single-threaded context.
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            })) {
                Ok(()) => println!("ok"),
                Err(panic) => {
                    println!("FAILED ({panic:?})");
                    failures.push(name);
                }
            }
        };

    // Full normalized matrix: segment creation, frame write/sync,
    // transaction, publish. The committed batch is line/raw chunk "two";
    // everything before its visibility transaction must stay invisible.
    // Creation-phase points run sealed (batch two walks a fresh segment:
    // a zero-committed pending row, tombstoned whole once recovery runs);
    // frame/transaction/publish points run in-stream (batch two lands
    // behind a committed boundary, so its uncommitted tail is quarantined
    // back to that boundary).
    for (name, seal, case) in [
        (
            "segment-before",
            true,
            Case {
                classification: Classification::Clean,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "segment-row-inserted",
            true,
            Case {
                classification: Classification::Tombstone,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "segment-header-written",
            true,
            Case {
                classification: Classification::Tombstone,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "segment-header-synced",
            true,
            Case {
                classification: Classification::Tombstone,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "frame-before-write",
            false,
            Case {
                classification: Classification::Clean,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "frame-after-write",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "frame-after-sync",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "txn-begin",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "txn-update",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "txn-before-commit",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 1,
                committed: b"one",
            },
        ),
        (
            "txn-after-commit",
            false,
            Case {
                classification: Classification::Clean,
                watermark: 2,
                committed: b"onetwo",
            },
        ),
        (
            "publish-before",
            false,
            Case {
                classification: Classification::Clean,
                watermark: 2,
                committed: b"onetwo",
            },
        ),
        (
            "publish-after",
            false,
            Case {
                classification: Classification::Clean,
                watermark: 2,
                committed: b"onetwo",
            },
        ),
    ] {
        run(name, Box::pin(run_case(name, false, seal, case)));
    }

    // Raw-stream subset: the same durability semantics on the other kind.
    for (name, seal, case) in [
        (
            "segment-row-inserted",
            true,
            Case {
                classification: Classification::Tombstone,
                watermark: 3,
                committed: b"one",
            },
        ),
        (
            "frame-after-sync",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 3,
                committed: b"one",
            },
        ),
        (
            "txn-before-commit",
            false,
            Case {
                classification: Classification::Tail,
                watermark: 3,
                committed: b"one",
            },
        ),
        (
            "txn-after-commit",
            false,
            Case {
                classification: Classification::Clean,
                watermark: 6,
                committed: b"onetwo",
            },
        ),
    ] {
        run(name, Box::pin(run_case(name, true, seal, case)));
    }

    // Power-loss samples: explicit physical truncation after the death.
    run(
        "power-loss-zero",
        Box::pin(run_power_case(
            "power-loss-zero",
            "frame-after-write",
            true,
            "zero",
        )),
    );
    run(
        "power-loss-header-zero",
        Box::pin(run_power_case(
            "power-loss-header-zero",
            "segment-header-written",
            true,
            "zero",
        )),
    );
    run(
        "power-loss-torn",
        Box::pin(run_power_case(
            "power-loss-torn",
            "frame-after-sync",
            false,
            "torn",
        )),
    );
    run(
        "power-loss-committed",
        Box::pin(run_power_case(
            "power-loss-committed",
            "txn-after-commit",
            true,
            "committed",
        )),
    );

    // A crash inside recovery itself (artifact durable, truncation pending).
    run(
        "recovery-crash",
        Box::pin(recovery_crash_between_artifact_and_truncation()),
    );

    // A crash inside recovery's loss transaction commit: the gap, pointer
    // clear, tombstone, and degraded latch are all-or-nothing.
    run(
        "recovery-loss-crash",
        Box::pin(recovery_crash_inside_loss_commit()),
    );

    // The exclusive writer lease across processes.
    run(
        "writer-lease-cross-process",
        Box::pin(writer_lease_is_exclusive_across_processes()),
    );

    if failures.is_empty() {
        println!("crash matrix: all cases passed");
    } else {
        eprintln!("crash matrix failures: {failures:?}");
        std::process::exit(1);
    }
}
