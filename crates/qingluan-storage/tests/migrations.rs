//! Migration behavior through the seam: transactional apply (no half DDL,
//! no recorded version on failure) and a clean later upgrade, plus the
//! persisted format-version guard.

#![cfg(feature = "test-hooks")]

use std::path::{Path, PathBuf};

use qingluan_storage::{LogStore, StorageError};
use sqlx::Row;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ql-storage-s2-migrations-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
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

/// Copy the crate's migrations into a writable directory and optionally
/// append extra files.
fn migration_dir(root: &TempRoot, extra: &[(&str, &str)]) -> PathBuf {
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in std::fs::read_dir(&source).unwrap() {
        let entry = entry.unwrap();
        let to = dir.join(entry.file_name());
        std::fs::copy(entry.path(), &to).unwrap();
    }
    for (name, sql) in extra {
        std::fs::write(dir.join(name), sql).unwrap();
    }
    dir
}

async fn raw_pool(root: &TempRoot) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(root.db());
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

async fn applied_versions(root: &TempRoot) -> Vec<i64> {
    let pool = raw_pool(root).await;
    let versions: Vec<i64> = sqlx::query("SELECT version FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<i64, _>(0))
        .collect();
    pool.close().await;
    versions
}

async fn table_exists(root: &TempRoot, table: &str) -> bool {
    let pool = raw_pool(root).await;
    let found: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_optional(&pool)
            .await
            .unwrap();
    pool.close().await;
    found.is_some()
}

async fn table_columns(root: &TempRoot, table: &str) -> Vec<String> {
    let pool = raw_pool(root).await;
    let names: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();
    pool.close().await;
    names
}

#[tokio::test]
async fn initial_migration_carries_both_streams_and_no_s4_tail() {
    let root = TempRoot::new("initial-schema");
    LogStore::open(&root).await.unwrap();
    assert_eq!(applied_versions(&root).await, vec![1, 2, 3]);

    // Per-stream active pointers and watermarks on the terminal row.
    let terminal = table_columns(&root, "terminal").await;
    assert!(terminal.contains(&"active_normalized_segment".to_owned()));
    assert!(terminal.contains(&"active_raw_segment".to_owned()));
    assert!(terminal.contains(&"line_watermark".to_owned()));
    assert!(terminal.contains(&"raw_watermark".to_owned()));
    assert!(!terminal.contains(&"active_segment".to_owned()));

    // Stream-kind and per-kind range columns on the segment row.
    let segment = table_columns(&root, "segment").await;
    assert!(segment.contains(&"kind".to_owned()));
    assert!(segment.contains(&"first_line".to_owned()));
    assert!(segment.contains(&"last_line".to_owned()));
    assert!(segment.contains(&"first_offset".to_owned()));
    assert!(segment.contains(&"last_offset".to_owned()));

    // The recovery pass's stream-scoped gap table: one `kind` discriminator
    // plus a coordinate-agnostic exclusive range, so normalized line ranges
    // and raw byte offsets never merge.
    let log_gap = table_columns(&root, "log_gap").await;
    assert!(log_gap.contains(&"kind".to_owned()));
    assert!(log_gap.contains(&"range_start".to_owned()));
    assert!(log_gap.contains(&"range_end".to_owned()));
    assert!(!log_gap.contains(&"first_line".to_owned()));

    // The raw retained floor mirrors the normalized one.
    let terminal_columns = table_columns(&root, "terminal").await;
    assert!(terminal_columns.contains(&"retained_first_offset".to_owned()));
    assert!(terminal_columns.contains(&"retained_first_line".to_owned()));

    // The S4 tail-revision surface is deliberately absent from this slice.
    assert!(
        !table_exists(&root, "tail_revision").await,
        "tail_revision belongs to a later slice"
    );

    // The kind CHECK and the per-kind range shape are enforced by SQLite.
    let pool = raw_pool(&root).await;
    sqlx::query(
        "INSERT INTO terminal
             (session_source, external_session_id, terminal_id, terminal_uuid, log_epoch)
         VALUES ('s', 'e', 't', randomblob(16), randomblob(16))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let insert = |kind: &str, first_line: Option<i64>, first_offset: i64| {
        sqlx::query(
            "INSERT INTO segment
                 (session_source, external_session_id, terminal_id, kind, file_name,
                  first_line, last_line, first_offset, last_offset, created_ms)
             VALUES ('s', 'e', 't', ?1, ?2, ?3, ?3, ?4, ?4, 0)",
        )
        .bind(kind)
        .bind(format!("seg-test-{kind}-{first_offset}.log"))
        .bind(first_line)
        .bind(first_offset)
        .execute(&pool)
    };
    // Valid shapes: normalized anchored at a line, raw anchored at a byte
    // offset with no line numbers.
    insert("normalized", Some(1), 0).await.unwrap();
    insert("raw", None, 4096).await.unwrap();
    // Unknown kind.
    assert!(insert("other", None, 0).await.is_err());
    // Raw with a line number, normalized without one: both rejected.
    assert!(insert("raw", Some(5), 0).await.is_err());
    assert!(insert("normalized", None, 0).await.is_err());
    pool.close().await;
}

#[tokio::test]
async fn failing_migration_rolls_back_with_no_half_ddl_and_no_version() {
    let root = TempRoot::new("rollback");

    // Baseline: the real 0001 + 0002 + 0003 apply cleanly.
    let dir = migration_dir(&root, &[]);
    LogStore::open_with_migration_dir(&root, &dir)
        .await
        .unwrap();
    assert_eq!(applied_versions(&root).await, vec![1, 2, 3]);

    // A 0004 whose first statement succeeds and whose second fails: the
    // per-migration transaction must roll the whole file back.
    let bad = "CREATE TABLE migration_probe_a (x INTEGER);\nCREATE TABLE migration_probe_b (;";
    let dir = migration_dir(&root, &[("0004_bad.sql", bad)]);
    match LogStore::open_with_migration_dir(&root, &dir).await {
        Err(StorageError::Migration(_)) => {}
        other => panic!(
            "expected Migration error, got {:?}",
            other.map(|_| ()).map_err(|error| error.to_string())
        ),
    }
    assert!(
        !table_exists(&root, "migration_probe_a").await,
        "no half DDL may survive a failed migration"
    );
    assert_eq!(
        applied_versions(&root).await,
        vec![1, 2, 3],
        "failed version must not be recorded"
    );

    // The same DB upgrades cleanly once 0004 is fixed.
    let good = "CREATE TABLE migration_probe_ok (x INTEGER);";
    let dir = migration_dir(&root, &[("0004_bad.sql", good)]);
    LogStore::open_with_migration_dir(&root, &dir)
        .await
        .unwrap();
    assert_eq!(applied_versions(&root).await, vec![1, 2, 3, 4]);
    assert!(table_exists(&root, "migration_probe_ok").await);
}

#[tokio::test]
async fn unsupported_persisted_format_version_fails_loudly_at_open() {
    let root = TempRoot::new("format-version");
    LogStore::open(&root).await.unwrap();

    let pool = raw_pool(&root).await;
    sqlx::query("UPDATE meta SET value = '99' WHERE key = 'format_version'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    match LogStore::open(&root).await {
        Err(StorageError::FormatVersionUnsupported { found, supported }) => {
            assert_eq!(found, "99");
            assert_eq!(supported, "3");
        }
        other => panic!(
            "expected FormatVersionUnsupported, got {:?}",
            other.map(|_| ()).map_err(|error| error.to_string())
        ),
    }
}

#[tokio::test]
async fn runtime_registry_migration_has_no_pid_cgroup_env_or_events() {
    let root = TempRoot::new("runtime-schema");
    LogStore::open(&root).await.unwrap();
    assert_eq!(applied_versions(&root).await, vec![1, 2, 3]);

    let columns = table_columns(&root, "terminal_runtime").await;
    for required in [
        "phase",
        "process_state",
        "exit_kind",
        "exit_value",
        "output_state",
        "output_end",
        "stopping",
        "size_rows",
        "size_columns",
        "revision",
    ] {
        assert!(
            columns.contains(&required.to_owned()),
            "terminal_runtime must carry {required}"
        );
    }
    // No OS resource handle, environment, or event surface leaks into the
    // registry: correlation is by identity components only.
    for forbidden in [
        "pid",
        "cgroup",
        "cgroup_path",
        "env",
        "environment",
        "event_seq",
        "event",
    ] {
        assert!(
            !columns.contains(&forbidden.to_owned()),
            "terminal_runtime must not carry {forbidden}"
        );
    }
    assert!(
        !table_exists(&root, "terminal_runtime_event").await,
        "the runtime registry is not an event log"
    );

    // The shape CHECK constraints reject fabricated or half-known states.
    let pool = raw_pool(&root).await;
    let mut next_terminal = 0u32;
    let mut insert = |phase: &str,
                      process: &str,
                      kind: Option<&str>,
                      value: Option<i64>,
                      output: &str,
                      end: Option<&str>| {
        next_terminal += 1;
        // A fresh key per insert so the UNIQUE primary key never interferes
        // with the shape checks under test.
        let sql = format!(
            "INSERT INTO terminal_runtime
                 (session_source, external_session_id, terminal_id, phase, process_state,
                  exit_kind, exit_value, output_state, output_end, stopping,
                  size_rows, size_columns, revision, created_ms, updated_ms)
             VALUES ('s', 'e', 't{next_terminal}', ?1, ?2, ?3, ?4, ?5, ?6, 0, 30, 120, 1, 0, 0)"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(phase)
            .bind(process)
            .bind(kind)
            .bind(value)
            .bind(output)
            .bind(end)
            .execute(&pool)
    };
    // A valid starting record and a valid interrupted one.
    insert("starting", "running", None, None, "open", None)
        .await
        .unwrap();
    insert(
        "cleaning",
        "interrupted",
        None,
        None,
        "closed",
        Some("interrupted"),
    )
    .await
    .unwrap();
    // Unknown phase / process / output values.
    assert!(
        insert("finished", "running", None, None, "open", None)
            .await
            .is_err()
    );
    assert!(
        insert("running", "gone", None, None, "open", None)
            .await
            .is_err()
    );
    assert!(
        insert("running", "running", None, None, "half", None)
            .await
            .is_err()
    );
    // Exited without a kind/value, and running with one: both rejected.
    assert!(
        insert("running", "exited", None, None, "open", None)
            .await
            .is_err()
    );
    assert!(
        insert("running", "running", Some("code"), Some(0), "open", None)
            .await
            .is_err()
    );
    // Closed without an end, and open with one: both rejected.
    assert!(
        insert("running", "running", None, None, "closed", None)
            .await
            .is_err()
    );
    assert!(
        insert("running", "running", None, None, "open", Some("eof"))
            .await
            .is_err()
    );
    // Zero size is rejected.
    assert!(
        sqlx::query(
            "INSERT INTO terminal_runtime
                 (session_source, external_session_id, terminal_id, phase, process_state,
                  output_state, stopping, size_rows, size_columns, revision, created_ms, updated_ms)
             VALUES ('s', 'e', 'z', 'starting', 'running', 'open', 0, 0, 120, 1, 0, 0)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    pool.close().await;
}
