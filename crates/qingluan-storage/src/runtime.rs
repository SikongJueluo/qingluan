//! Durable terminal-runtime registry (S3).
//!
//! One narrow, concrete SQLite-backed table (`terminal_runtime`, migration
//! 0003) records the durable lifecycle of each terminal so a daemon start
//! can find records left behind by a crash. It is deliberately minimal: a
//! phase, the independent process and output dimensions, a stop latch, the
//! window size, and a monotonic revision. It stores **no** PID, **no**
//! cgroup path, **no** environment snapshot, and **no** lifecycle event —
//! correlation with OS resources happens through the terminal identity
//! components alone, so recovery can mark a record `Interrupted` without
//! ever being tempted to signal a process it only knows by a stale pid.
//!
//! Phase transitions (all atomic, all bumping `revision`):
//!
//! ```text
//! starting --running--> running --stop_intent--> cleaning --released--> released
//!    |                     |                                             ^
//!    |                process_exit / output_close (independent)          |
//!    +--rollback_starting-----------------------------------------------+
//! ```
//!
//! `process_exit` and `output_close` are independent dimensions with no
//! ordering between them; each is idempotent for a repeated equal value
//! and refuses to overwrite a different known outcome. `released` frees the
//! quota slot and is idempotent. `rollback_starting` is the failed-start
//! path (`starting` -> `released`) and refuses on any other phase.
//! [`RuntimeRegistry::interrupt_unfinished`] atomically marks every
//! unfinished record Interrupted (never a fabricated exit) and moves it to
//! `cleaning`; it is idempotent, never signals, and leaves `released`
//! records untouched.

use std::path::Path;
use std::sync::Arc;

use qingluan_core::terminal::{
    ExitResult, OutputEnd, OutputState, ProcessState, TerminalRef, TerminalSize,
};
use sqlx::{Row, SqlitePool};

use crate::db::Store;
use crate::error::{StorageError, db_error, i64_of};
use crate::paths;

/// Lifecycle phase of one terminal's runtime record.
///
/// `starting`, `running`, and `cleaning` all occupy a quota slot; only
/// `released` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    /// The slot is reserved; the root process is not yet confirmed started.
    Starting,
    /// The root process has been confirmed started.
    Running,
    /// A stop is committed, or an unfinished record was found after a
    /// restart; resources are not yet reclaimed.
    Cleaning,
    /// Resources are reclaimed and the slot is free.
    Released,
}

impl RuntimePhase {
    fn from_text(text: &str) -> Result<Self, StorageError> {
        match text {
            "starting" => Ok(RuntimePhase::Starting),
            "running" => Ok(RuntimePhase::Running),
            "cleaning" => Ok(RuntimePhase::Cleaning),
            "released" => Ok(RuntimePhase::Released),
            other => Err(StorageError::Database(format!(
                "unknown runtime phase {other:?}"
            ))),
        }
    }
}

/// One durable runtime record, in domain types (no sqlx or wire type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRecord {
    /// Which terminal this record describes.
    pub terminal: TerminalRef,
    /// Lifecycle phase.
    pub phase: RuntimePhase,
    /// Root-process dimension (independent of `output`).
    pub process: ProcessState,
    /// Output-read dimension (independent of `process`).
    pub output: OutputState,
    /// A stop flow has been committed.
    pub stopping: bool,
    /// Last known window size.
    pub size: TerminalSize,
    /// Monotonic revision, bumped by every change (starts at 1).
    pub revision: u64,
}

/// Durable terminal-runtime registry over one storage root.
///
/// Shares `terminal.db` with [`crate::LogStore`] (both open the same
/// versioned schema). Cheap to clone.
#[derive(Clone)]
pub struct RuntimeRegistry {
    store: Arc<Store>,
}

type KeyRow = (String, String, String);

fn key_of(terminal: &TerminalRef) -> KeyRow {
    (
        terminal.session.source.as_str().to_owned(),
        terminal.session.external_id.as_str().to_owned(),
        terminal.terminal_id.as_str().to_owned(),
    )
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<RuntimeRecord, StorageError> {
    let phase_text: String = row.try_get("phase").map_err(db_error)?;
    let process_text: String = row.try_get("process_state").map_err(db_error)?;
    let output_text: String = row.try_get("output_state").map_err(db_error)?;
    let exit_kind: Option<String> = row.try_get("exit_kind").map_err(db_error)?;
    let exit_value: Option<i64> = row.try_get("exit_value").map_err(db_error)?;
    let output_end: Option<String> = row.try_get("output_end").map_err(db_error)?;

    let process = match process_text.as_str() {
        "running" => ProcessState::Running,
        "exited" => {
            let value = i32::try_from(exit_value.ok_or_else(|| {
                StorageError::Database("exited process without an exit value".into())
            })?)
            .map_err(|_| StorageError::Database("exit value out of range".into()))?;
            match exit_kind.as_deref() {
                Some("code") => ProcessState::Exited(ExitResult::ExitCode(value)),
                Some("signal") => ProcessState::Exited(ExitResult::Signal(value)),
                other => {
                    return Err(StorageError::Database(format!(
                        "exited process without a known exit kind: {other:?}"
                    )));
                }
            }
        }
        "interrupted" => ProcessState::Interrupted,
        other => {
            return Err(StorageError::Database(format!(
                "unknown process state {other:?}"
            )));
        }
    };

    let output = match output_text.as_str() {
        "open" => OutputState::Open,
        "closed" => {
            let end = match output_end.as_deref() {
                Some("eof") => OutputEnd::Eof,
                Some("forced") => OutputEnd::ForcedClose,
                Some("read_error") => OutputEnd::ReadError,
                Some("interrupted") => OutputEnd::Interrupted,
                other => {
                    return Err(StorageError::Database(format!(
                        "closed output without a known end: {other:?}"
                    )));
                }
            };
            OutputState::Closed(end)
        }
        other => {
            return Err(StorageError::Database(format!(
                "unknown output state {other:?}"
            )));
        }
    };

    let stopping: i64 = row.try_get("stopping").map_err(db_error)?;
    let rows: i64 = row.try_get("size_rows").map_err(db_error)?;
    let columns: i64 = row.try_get("size_columns").map_err(db_error)?;

    Ok(RuntimeRecord {
        terminal: TerminalRef {
            session: qingluan_core::terminal::SessionRef {
                source: qingluan_core::terminal::SessionSource::new(
                    row.try_get::<String, _>("session_source")
                        .map_err(db_error)?,
                ),
                external_id: qingluan_core::terminal::ExternalSessionId::new(
                    row.try_get::<String, _>("external_session_id")
                        .map_err(db_error)?,
                ),
            },
            terminal_id: qingluan_core::terminal::TerminalId::new(
                row.try_get::<String, _>("terminal_id").map_err(db_error)?,
            ),
        },
        phase: RuntimePhase::from_text(&phase_text)?,
        process,
        output,
        stopping: stopping != 0,
        size: TerminalSize {
            rows: u16::try_from(rows)
                .map_err(|_| StorageError::Database("size_rows out of range".into()))?,
            columns: u16::try_from(columns)
                .map_err(|_| StorageError::Database("size_columns out of range".into()))?,
        },
        revision: u64::try_from(row.try_get::<i64, _>("revision").map_err(db_error)?)
            .map_err(|_| StorageError::Database("negative revision".into()))?,
    })
}

/// Which exit columns an [`ExitResult`] writes.
fn exit_columns(result: ExitResult) -> Result<(&'static str, i64), StorageError> {
    match result {
        ExitResult::ExitCode(code) => Ok(("code", i64::from(code))),
        ExitResult::Signal(signal) => Ok(("signal", i64::from(signal))),
        // `ExitResult` is `#[non_exhaustive]`: an outcome this build does not
        // know how to persist must be refused, never stored as a bogus kind.
        _ => Err(StorageError::RuntimeConflict {
            detail: "unsupported exit outcome".into(),
        }),
    }
}

fn output_end_text(end: OutputEnd) -> Result<&'static str, StorageError> {
    match end {
        OutputEnd::Eof => Ok("eof"),
        OutputEnd::ForcedClose => Ok("forced"),
        OutputEnd::ReadError => Ok("read_error"),
        OutputEnd::Interrupted => Ok("interrupted"),
        _ => Err(StorageError::RuntimeConflict {
            detail: "unsupported output end".into(),
        }),
    }
}

impl RuntimeRegistry {
    /// Build a registry over an already-open store (one shared connection).
    /// See `LogStore::runtime_registry`, which is the intended caller.
    pub(crate) fn from_shared(store: Arc<Store>) -> Self {
        RuntimeRegistry { store }
    }

    /// Open (creating if needed) the storage root's database and apply the
    /// versioned migrations.
    pub async fn open(root: &Path) -> Result<RuntimeRegistry, StorageError> {
        Ok(RuntimeRegistry {
            store: Store::open(root).await?,
        })
    }

    fn pool(&self) -> &SqlitePool {
        self.store.pool()
    }

    fn conflict(detail: impl Into<String>) -> StorageError {
        StorageError::RuntimeConflict {
            detail: detail.into(),
        }
    }

    async fn load_pool(&self, key: &KeyRow) -> Result<Option<RuntimeRecord>, StorageError> {
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_optional(self.pool())
            .await
            .map_err(db_error)?;
        row.as_ref().map(row_to_record).transpose()
    }

    /// Reserve a slot: insert a fresh `starting` record (process `running`,
    /// output `open`, `stopping` unset, revision 1). Fails with a typed
    /// conflict when a record already exists for this terminal (a terminal
    /// id is never reused).
    pub async fn begin(
        &self,
        terminal: &TerminalRef,
        size: TerminalSize,
    ) -> Result<RuntimeRecord, StorageError> {
        if size.rows == 0 || size.columns == 0 {
            return Err(Self::conflict("terminal size must be non-zero"));
        }
        let key = key_of(terminal);
        let now = i64_of(paths::now_ms())?;
        let result = sqlx::query(
            "INSERT INTO terminal_runtime
                 (session_source, external_session_id, terminal_id, phase, process_state,
                  output_state, stopping, size_rows, size_columns, revision, created_ms, updated_ms)
             VALUES (?1, ?2, ?3, 'starting', 'running', 'open', 0, ?4, ?5, 1, ?6, ?6)
             ON CONFLICT DO NOTHING",
        )
        .bind(&key.0)
        .bind(&key.1)
        .bind(&key.2)
        .bind(i64::from(size.rows))
        .bind(i64::from(size.columns))
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(Self::conflict(format!(
                "terminal {} already has a runtime record",
                key.2
            )));
        }
        self.load_pool(&key)
            .await?
            .ok_or_else(|| Self::conflict("begin inserted no row"))
    }

    /// Confirm the root process started: `starting` -> `running`. Idempotent
    /// when already `running`.
    pub async fn mark_running(
        &self,
        terminal: &TerminalRef,
    ) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let updated = self
            .update(
                &key,
                "UPDATE terminal_runtime
                    SET phase = 'running', revision = revision + 1, updated_ms = ?4
                  WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                    AND phase = 'starting'",
            )
            .await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        match existing.phase {
            RuntimePhase::Running => Ok(existing),
            other => Err(Self::conflict(format!(
                "mark_running requires a starting record, found {other:?}"
            ))),
        }
    }

    /// Commit stop intent: set the stop latch and enter `cleaning` from
    /// `starting` or `running`. Idempotent once `cleaning`.
    pub async fn stop_intent(&self, terminal: &TerminalRef) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let updated = self
            .update(
                &key,
                "UPDATE terminal_runtime
                    SET stopping = 1, phase = 'cleaning', revision = revision + 1, updated_ms = ?4
                  WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                    AND phase IN ('starting', 'running')",
            )
            .await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        match existing.phase {
            RuntimePhase::Cleaning => Ok(existing),
            other => Err(Self::conflict(format!(
                "stop_intent requires a starting or running record, found {other:?}"
            ))),
        }
    }

    /// Record a known root-process exit. Idempotent for the same result;
    /// refuses to overwrite a different exit or an `Interrupted` process
    /// (an unknown outcome is never replaced by a fabricated exit).
    pub async fn process_exit(
        &self,
        terminal: &TerminalRef,
        result: ExitResult,
    ) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let (kind, value) = exit_columns(result)?;
        let updated = self.update_with_exit(&key, kind, value).await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        match existing.process {
            ProcessState::Exited(known) if known == result => Ok(existing),
            ProcessState::Exited(known) => Err(Self::conflict(format!(
                "process already exited as {known:?}, cannot overwrite with {result:?}"
            ))),
            ProcessState::Interrupted => Err(Self::conflict(
                "process outcome is interrupted; refusing to fabricate an exit",
            )),
            ProcessState::Running => Err(Self::conflict("process_exit matched no record")),
            // `ProcessState` is `#[non_exhaustive]`: refuse an unknown state
            // rather than guess.
            _ => Err(Self::conflict("unknown process state")),
        }
    }

    /// Record that output reading ended. Idempotent for the same end;
    /// refuses to overwrite a different known end.
    pub async fn output_close(
        &self,
        terminal: &TerminalRef,
        end: OutputEnd,
    ) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let updated = self.update_output(&key, output_end_text(end)?).await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        match existing.output {
            OutputState::Closed(known) if known == end => Ok(existing),
            OutputState::Closed(known) => Err(Self::conflict(format!(
                "output already closed as {known:?}, cannot overwrite with {end:?}"
            ))),
            OutputState::Open => Err(Self::conflict("output_close matched no record")),
            // `OutputState` is `#[non_exhaustive]`: refuse an unknown state.
            _ => Err(Self::conflict("unknown output state")),
        }
    }

    /// Reclaim the slot: any non-`released` phase -> `released`. Idempotent.
    pub async fn released(&self, terminal: &TerminalRef) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let updated = self
            .update(
                &key,
                "UPDATE terminal_runtime
                    SET phase = 'released', revision = revision + 1, updated_ms = ?4
                  WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                    AND phase <> 'released'",
            )
            .await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        if existing.phase == RuntimePhase::Released {
            Ok(existing)
        } else {
            Err(Self::conflict("released matched no record"))
        }
    }

    /// Roll back a failed start: `starting` -> `released`. Idempotent once
    /// released; refuses on any other phase (a running terminal must be
    /// stopped and reclaimed instead).
    pub async fn rollback_starting(
        &self,
        terminal: &TerminalRef,
    ) -> Result<RuntimeRecord, StorageError> {
        let key = key_of(terminal);
        let updated = self
            .update(
                &key,
                "UPDATE terminal_runtime
                    SET phase = 'released', revision = revision + 1, updated_ms = ?4
                  WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                    AND phase = 'starting'",
            )
            .await?;
        if let Some(record) = updated {
            return Ok(record);
        }
        let existing = self.require(&key).await?;
        match existing.phase {
            RuntimePhase::Released => Ok(existing),
            other => Err(Self::conflict(format!(
                "rollback_starting requires a starting record, found {other:?}"
            ))),
        }
    }

    /// The record for one terminal, if any.
    pub async fn load(
        &self,
        terminal: &TerminalRef,
    ) -> Result<Option<RuntimeRecord>, StorageError> {
        self.load_pool(&key_of(terminal)).await
    }

    /// Every runtime record, stable-ordered by creation then identity.
    pub async fn list(&self) -> Result<Vec<RuntimeRecord>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM terminal_runtime
              ORDER BY created_ms, session_source, external_session_id, terminal_id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_error)?;
        rows.iter().map(row_to_record).collect()
    }

    /// Atomically mark every unfinished record `Interrupted`.
    ///
    /// One transaction: each non-`released` record whose process is
    /// `running` becomes `interrupted`, each whose output is `open` becomes
    /// `closed` with end `interrupted`, and its phase becomes `cleaning`
    /// (so the slot stays occupied until cleanup is verified and
    /// [`RuntimeRegistry::released`] runs). A record that already has a
    /// known exit or output end keeps it — recovery never fabricates.
    /// Never signals a process. Idempotent: a second call changes nothing
    /// and returns no records, so consecutive recoveries observe an equal
    /// durable state. Returns the records actually changed, in list order.
    pub async fn interrupt_unfinished(&self) -> Result<Vec<RuntimeRecord>, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let keys: Vec<KeyRow> = sqlx::query(
            "SELECT session_source, external_session_id, terminal_id
               FROM terminal_runtime
              WHERE phase <> 'released'
                AND (process_state = 'running' OR output_state = 'open'
                     OR phase IN ('starting', 'running'))
              ORDER BY created_ms, session_source, external_session_id, terminal_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?
        .iter()
        .map(|row| {
            Ok::<_, StorageError>((
                row.try_get("session_source").map_err(db_error)?,
                row.try_get("external_session_id").map_err(db_error)?,
                row.try_get("terminal_id").map_err(db_error)?,
            ))
        })
        .collect::<Result<_, _>>()?;
        if keys.is_empty() {
            tx.commit().await.map_err(db_error)?;
            return Ok(Vec::new());
        }
        let now = i64_of(paths::now_ms())?;
        sqlx::query(
            "UPDATE terminal_runtime
                SET phase = 'cleaning',
                    process_state = CASE WHEN process_state = 'running'
                                         THEN 'interrupted' ELSE process_state END,
                    output_state = CASE WHEN output_state = 'open'
                                        THEN 'closed' ELSE output_state END,
                    output_end = CASE WHEN output_state = 'open'
                                      THEN 'interrupted' ELSE output_end END,
                    revision = revision + 1,
                    updated_ms = ?1
              WHERE phase <> 'released'
                AND (process_state = 'running' OR output_state = 'open'
                     OR phase IN ('starting', 'running'))",
        )
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut changed = Vec::with_capacity(keys.len());
        for key in &keys {
            let row = sqlx::query(SELECT_ONE)
                .bind(&key.0)
                .bind(&key.1)
                .bind(&key.2)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            changed.push(row_to_record(&row)?);
        }
        tx.commit().await.map_err(db_error)?;
        Ok(changed)
    }

    /// Run a conditional update (the WHERE clause encodes the legal
    /// transition) and return the updated record, or `None` when the
    /// condition matched no row. `revision = revision + 1` and `updated_ms`
    /// are part of the statement; the atomic row is re-read in the same
    /// transaction.
    async fn update(
        &self,
        key: &KeyRow,
        sql: &'static str,
    ) -> Result<Option<RuntimeRecord>, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let now = i64_of(paths::now_ms())?;
        let result = sqlx::query(sql)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if result.rows_affected() == 0 {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let record = row_to_record(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(record))
    }

    async fn update_with_exit(
        &self,
        key: &KeyRow,
        kind: &str,
        value: i64,
    ) -> Result<Option<RuntimeRecord>, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let now = i64_of(paths::now_ms())?;
        let result = sqlx::query(
            "UPDATE terminal_runtime
                SET process_state = 'exited', exit_kind = ?4, exit_value = ?5,
                    revision = revision + 1, updated_ms = ?6
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND process_state = 'running'",
        )
        .bind(&key.0)
        .bind(&key.1)
        .bind(&key.2)
        .bind(kind)
        .bind(value)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let record = row_to_record(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(record))
    }

    async fn update_output(
        &self,
        key: &KeyRow,
        end: &str,
    ) -> Result<Option<RuntimeRecord>, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let now = i64_of(paths::now_ms())?;
        let result = sqlx::query(
            "UPDATE terminal_runtime
                SET output_state = 'closed', output_end = ?4,
                    revision = revision + 1, updated_ms = ?5
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND output_state = 'open'",
        )
        .bind(&key.0)
        .bind(&key.1)
        .bind(&key.2)
        .bind(end)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let record = row_to_record(&row)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Some(record))
    }

    async fn require(&self, key: &KeyRow) -> Result<RuntimeRecord, StorageError> {
        self.load_pool(key)
            .await?
            .ok_or_else(|| Self::conflict(format!("terminal {} has no runtime record", key.2)))
    }
}

const SELECT_ONE: &str = "SELECT * FROM terminal_runtime
     WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3";
