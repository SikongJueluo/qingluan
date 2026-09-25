//! Durable terminal-runtime registry (S3) and per-session lifecycle event
//! stream (S5).
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
//! The lifecycle events themselves live in a parallel per-**session**
//! stream (`session_event` + `session_state`, migration 0004), keyed by
//! the composite `(session_source, external_session_id)` identity: one
//! session shares one waterline across all of its terminals. Each event
//! carries its `terminal_id` and complete typed payload, so replay stays
//! self-explanatory after the terminal record is deleted (there is no
//! foreign key and no cascade).
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
//! and refuses to overwrite a different known outcome. When a transition
//! does happen, the terminal state change and its one corresponding
//! `session_event` (plus the `last_committed_seq` bump) commit in **one**
//! transaction, allocated from that persistent counter rather than from
//! `MAX(event_seq)`, so a full prune can never reuse a published sequence.
//! [`RuntimeRegistry::process_exit_committed`] and
//! [`RuntimeRegistry::output_close_committed`] hand the committed event
//! back (the post-commit publication seam); the plain methods return only
//! the record, so the terminal runtime persists events without publishing
//! any uncommitted one. `released` frees the quota slot and is idempotent.
//! `rollback_starting` is the failed-start path (`starting` -> `released`)
//! and refuses on any other phase.
//! [`RuntimeRegistry::interrupt_unfinished`] atomically marks every
//! unfinished record Interrupted (never a fabricated exit) and moves it to
//! `cleaning`; it is idempotent, never signals, and leaves `released`
//! records untouched.

use std::path::Path;
use std::sync::Arc;

use qingluan_core::terminal::{
    EventSequence, ExitResult, ExternalSessionId, OutputEnd, OutputState, ProcessState,
    SessionEvent, SessionEventPayload, SessionEventState, SessionRef, SessionSource, TerminalId,
    TerminalRef, TerminalSize,
};
use sqlx::{Row, SqlitePool};

use crate::crash::CrashPoint;
use crate::db::Store;
use crate::error::{StorageError, db_error, i64_of, u64_of};
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

/// One committed lifecycle transition and the session event it allocated.
///
/// The event is `Some` only when this call performed the transition (and so
/// committed the event in the same transaction); an idempotent repeat of an
/// already-recorded transition returns `None` and allocates no new sequence.
/// Handing the pair back is the explicit post-commit publication seam: it is
/// only built after `tx.commit()`, so an event can never be published
/// before it is durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleCommit {
    /// The durable runtime record after the transition.
    pub record: RuntimeRecord,
    /// The event committed in the same transaction, if this call allocated one.
    pub event: Option<SessionEvent>,
}

/// A bounded page of a session's committed lifecycle events plus a
/// consistent watermark snapshot.
///
/// `events` is strictly ascending and strictly after the requested
/// `after_event_seq`; it never extends past the snapshot's
/// `last_committed_seq`, so a page cut is fixed against later appends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReplay {
    /// Watermarks read in the same transaction as `events`.
    pub state: SessionEventState,
    /// Ordered page: at most [`MAX_EVENT_PAGE`] events.
    pub events: Vec<SessionEvent>,
    /// Continuation metadata: pass this as `after_event_seq` to fetch the
    /// next page. `None` means the page reached `last_committed_seq`.
    pub next_after_seq: Option<u64>,
}

/// Hard cap on the events returned by one [`RuntimeRegistry::events_after`]
/// page. A page is never an unbounded `Vec`.
pub const MAX_EVENT_PAGE: usize = 256;

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

/// Typed columns a [`SessionEventPayload`] writes to `session_event`.
struct EventColumns {
    kind: &'static str,
    exit_kind: Option<&'static str>,
    exit_value: Option<i64>,
    output_end: Option<&'static str>,
}

/// Map a payload to its typed columns. `#[non_exhaustive]` payloads this
/// build does not know are refused (typed conflict), never stored as a
/// bogus kind.
fn event_columns(payload: SessionEventPayload) -> Result<EventColumns, StorageError> {
    match payload {
        SessionEventPayload::ProcessExited(result) => {
            let (kind, value) = exit_columns(result)?;
            Ok(EventColumns {
                kind: "exited",
                exit_kind: Some(kind),
                exit_value: Some(value),
                output_end: None,
            })
        }
        SessionEventPayload::OutputClosed(end) => Ok(EventColumns {
            kind: "output_closed",
            exit_kind: None,
            exit_value: None,
            output_end: Some(output_end_text(end)?),
        }),
        _ => Err(StorageError::RuntimeConflict {
            detail: "unsupported session event payload".into(),
        }),
    }
}

/// Decode one `session_event` row back into the domain type. A row this
/// build cannot interpret (unknown kind, missing/inconsistent payload, a
/// value outside the domain range) is refused with a typed database
/// error, never coerced.
fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<SessionEvent, StorageError> {
    let session = SessionRef {
        source: SessionSource::new(
            row.try_get::<String, _>("session_source")
                .map_err(db_error)?,
        ),
        external_id: ExternalSessionId::new(
            row.try_get::<String, _>("external_session_id")
                .map_err(db_error)?,
        ),
    };
    let terminal_id = TerminalId::new(row.try_get::<String, _>("terminal_id").map_err(db_error)?);
    let raw_seq: i64 = row.try_get("event_seq").map_err(db_error)?;
    let seq = u64_of(raw_seq)?;
    let event_seq = EventSequence::new(seq)
        .ok_or_else(|| StorageError::Database("session event carries event_seq 0".into()))?;
    let kind: String = row.try_get("kind").map_err(db_error)?;
    let payload = match kind.as_str() {
        "exited" => {
            let exit_kind: Option<String> = row.try_get("exit_kind").map_err(db_error)?;
            let exit_value: Option<i64> = row.try_get("exit_value").map_err(db_error)?;
            let value = i32::try_from(exit_value.ok_or_else(|| {
                StorageError::Database("exited event without an exit value".into())
            })?)
            .map_err(|_| StorageError::Database("event exit value out of range".into()))?;
            let result = match exit_kind.as_deref() {
                Some("code") => ExitResult::ExitCode(value),
                Some("signal") => ExitResult::Signal(value),
                other => {
                    return Err(StorageError::Database(format!(
                        "exited event without a known exit kind: {other:?}"
                    )));
                }
            };
            SessionEventPayload::ProcessExited(result)
        }
        "output_closed" => {
            let output_end: Option<String> = row.try_get("output_end").map_err(db_error)?;
            let end = match output_end.as_deref() {
                Some("eof") => OutputEnd::Eof,
                Some("forced") => OutputEnd::ForcedClose,
                Some("read_error") => OutputEnd::ReadError,
                Some("interrupted") => OutputEnd::Interrupted,
                other => {
                    return Err(StorageError::Database(format!(
                        "closed event without a known output end: {other:?}"
                    )));
                }
            };
            SessionEventPayload::OutputClosed(end)
        }
        other => {
            return Err(StorageError::Database(format!(
                "unknown session event kind {other:?}"
            )));
        }
    };
    Ok(SessionEvent {
        terminal: TerminalRef {
            session,
            terminal_id,
        },
        event_seq,
        payload,
    })
}

/// Read one session's watermarks, or `None` when no state row exists yet.
async fn read_state_row(
    conn: &mut sqlx::SqliteConnection,
    session: &SessionRef,
) -> Result<Option<(u64, u64, u64)>, StorageError> {
    let row: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT pruned_through_seq, acked_through_seq, last_committed_seq
           FROM session_state
          WHERE session_source = ?1 AND external_session_id = ?2",
    )
    .bind(session.source.as_str())
    .bind(session.external_id.as_str())
    .fetch_optional(conn)
    .await
    .map_err(db_error)?;
    match row {
        Some((pruned, acked, committed)) => {
            Ok(Some((u64_of(pruned)?, u64_of(acked)?, u64_of(committed)?)))
        }
        None => Ok(None),
    }
}

fn watermark_state(
    pruned: u64,
    acked: u64,
    committed: u64,
) -> Result<SessionEventState, StorageError> {
    SessionEventState::new(pruned, acked, committed).map_err(|error| {
        StorageError::Database(format!("session watermark invariant violated: {error}"))
    })
}

/// Allocate the next `event_seq` for one session **from the persistent
/// `session_state.last_committed_seq` counter** (never from
/// `MAX(event_seq)`), insert the event row, and advance the watermark — all
/// on the caller's transaction, so the terminal state change and the event
/// commit or roll back together.
///
/// A full prune empties `session_event` but never resets
/// `last_committed_seq`, so the next allocation continues past every
/// already-published sequence. SQL values are bound/read through checked
/// signed conversions.
async fn allocate_event(
    conn: &mut sqlx::SqliteConnection,
    session: &SessionRef,
    terminal_id: &TerminalId,
    payload: SessionEventPayload,
) -> Result<SessionEvent, StorageError> {
    let columns = event_columns(payload)?;
    let last = read_state_row(conn, session)
        .await?
        .map(|(_, _, committed)| committed)
        .unwrap_or(0);
    let next = last
        .checked_add(1)
        .ok_or_else(|| StorageError::Database("session event sequence exhausted".into()))?;
    let event_seq = EventSequence::new(next)
        .ok_or_else(|| StorageError::Database("session event sequence wrapped to 0".into()))?;
    let now = i64_of(paths::now_ms())?;
    sqlx::query(
        "INSERT INTO session_event
             (session_source, external_session_id, terminal_id, event_seq, kind,
              exit_kind, exit_value, output_end, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(session.source.as_str())
    .bind(session.external_id.as_str())
    .bind(terminal_id.as_str())
    .bind(i64_of(next)?)
    .bind(columns.kind)
    .bind(columns.exit_kind)
    .bind(columns.exit_value)
    .bind(columns.output_end)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO session_state
             (session_source, external_session_id, pruned_through_seq,
              acked_through_seq, last_committed_seq)
         VALUES (?1, ?2, 0, 0, ?3)
         ON CONFLICT (session_source, external_session_id) DO UPDATE
            SET last_committed_seq = MAX(session_state.last_committed_seq,
                                         excluded.last_committed_seq)",
    )
    .bind(session.source.as_str())
    .bind(session.external_id.as_str())
    .bind(i64_of(next)?)
    .execute(&mut *conn)
    .await
    .map_err(db_error)?;
    Ok(SessionEvent {
        terminal: TerminalRef {
            session: session.clone(),
            terminal_id: terminal_id.clone(),
        },
        event_seq,
        payload,
    })
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
    ///
    /// The durable state change and the corresponding `ProcessExited`
    /// session event are committed in **one** transaction; the returned
    /// record is the pre-existing one on a duplicate. Callers that need the
    /// committed event (the explicit post-commit publication seam) use
    /// [`RuntimeRegistry::process_exit_committed`].
    pub async fn process_exit(
        &self,
        terminal: &TerminalRef,
        result: ExitResult,
    ) -> Result<RuntimeRecord, StorageError> {
        Ok(self.process_exit_committed(terminal, result).await?.record)
    }

    /// [`RuntimeRegistry::process_exit`] returning the committed
    /// [`SessionEvent`] alongside the record.
    ///
    /// `event` is `Some` exactly when this call performed the transition:
    /// the terminal's `process_state` flip and the event's sequence
    /// allocation commit in one transaction, after which the pair is
    /// returned (the publication seam). A repeated equal exit is idempotent
    /// and allocates **no** second event; a conflicting exit stays a typed
    /// failure and allocates nothing.
    pub async fn process_exit_committed(
        &self,
        terminal: &TerminalRef,
        result: ExitResult,
    ) -> Result<LifecycleCommit, StorageError> {
        let key = key_of(terminal);
        if let Some((record, event)) = self.update_with_exit(terminal, &key, result).await? {
            // The transaction is durable by now; only after this point is
            // the committed event published to the caller.
            self.store.hit(CrashPoint::EventPublish);
            return Ok(LifecycleCommit {
                record,
                event: Some(event),
            });
        }
        let existing = self.require(&key).await?;
        match existing.process {
            ProcessState::Exited(known) if known == result => Ok(LifecycleCommit {
                record: existing,
                event: None,
            }),
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
    ///
    /// As with [`RuntimeRegistry::process_exit`], the durable state change
    /// and the corresponding `OutputClosed` event commit in one
    /// transaction. Use [`RuntimeRegistry::output_close_committed`] for the
    /// post-commit publication seam.
    pub async fn output_close(
        &self,
        terminal: &TerminalRef,
        end: OutputEnd,
    ) -> Result<RuntimeRecord, StorageError> {
        Ok(self.output_close_committed(terminal, end).await?.record)
    }

    /// [`RuntimeRegistry::output_close`] returning the committed
    /// [`SessionEvent`] alongside the record. `event` is `Some` exactly when
    /// this call performed the transition; a repeated equal end is
    /// idempotent and allocates no second event.
    pub async fn output_close_committed(
        &self,
        terminal: &TerminalRef,
        end: OutputEnd,
    ) -> Result<LifecycleCommit, StorageError> {
        let key = key_of(terminal);
        if let Some((record, event)) = self.update_output(terminal, &key, end).await? {
            self.store.hit(CrashPoint::EventPublish);
            return Ok(LifecycleCommit {
                record,
                event: Some(event),
            });
        }
        let existing = self.require(&key).await?;
        match existing.output {
            OutputState::Closed(known) if known == end => Ok(LifecycleCommit {
                record: existing,
                event: None,
            }),
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

    /// The session's cumulative event watermarks. A session with no events
    /// yet reports the coherent all-zero state; no row is fabricated.
    pub async fn event_state(
        &self,
        session: &SessionRef,
    ) -> Result<SessionEventState, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let row = read_state_row(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        let (pruned, acked, committed) = row.unwrap_or((0, 0, 0));
        watermark_state(pruned, acked, committed)
    }

    /// One bounded, ordered page of the session's committed lifecycle
    /// events, strictly after `after_event_seq`.
    ///
    /// The page and its [`SessionEventState`] snapshot are read on one
    /// transaction and clamped to that snapshot's `last_committed_seq`, so
    /// later appends cannot extend a page already returned. At most
    /// [`MAX_EVENT_PAGE`] events are returned; `next_after_seq` carries the
    /// continuation when more remain.
    ///
    /// A request starting strictly before `pruned_through_seq` is refused
    /// with [`StorageError::EventRangeCleared`] (carrying the cleared prefix
    /// and the earliest recoverable resumption point) instead of silently
    /// jumping to the newest event. A missing session is an empty page over
    /// the all-zero state.
    pub async fn events_after(
        &self,
        session: &SessionRef,
        after_event_seq: u64,
    ) -> Result<EventReplay, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let (pruned, acked, committed) =
            read_state_row(&mut tx, session).await?.unwrap_or((0, 0, 0));
        if after_event_seq < pruned {
            tx.rollback().await.map_err(db_error)?;
            return Err(StorageError::EventRangeCleared {
                after: after_event_seq,
                pruned_through_seq: pruned,
                available_after_seq: pruned,
            });
        }
        let state = watermark_state(pruned, acked, committed)?;
        let limit = i64::try_from(MAX_EVENT_PAGE + 1).expect("page cap fits i64");
        let rows = sqlx::query(
            "SELECT session_source, external_session_id, terminal_id, event_seq, kind,
                    exit_kind, exit_value, output_end
               FROM session_event
              WHERE session_source = ?1 AND external_session_id = ?2
                AND event_seq > ?3 AND event_seq <= ?4
              ORDER BY event_seq
              LIMIT ?5",
        )
        .bind(session.source.as_str())
        .bind(session.external_id.as_str())
        .bind(i64_of(after_event_seq)?)
        .bind(i64_of(committed)?)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        let mut events = rows
            .iter()
            .map(row_to_event)
            .collect::<Result<Vec<_>, _>>()?;
        let next_after_seq = if events.len() > MAX_EVENT_PAGE {
            events.truncate(MAX_EVENT_PAGE);
            events.last().map(|event| event.event_seq.get())
        } else {
            None
        };
        tx.commit().await.map_err(db_error)?;
        Ok(EventReplay {
            state,
            events,
            next_after_seq,
        })
    }

    /// Cumulative acknowledgement of the session's events up to
    /// `up_to_seq`.
    ///
    /// Monotonic and bounded: the ack never regresses (repeats are
    /// harmless), and a bound beyond the durable `last_committed_seq` is
    /// refused with [`StorageError::EventAckOutOfBounds`] and writes
    /// nothing. A session with no events cannot be acknowledged beyond zero
    /// and no state row is fabricated. Returns the resulting watermark
    /// state.
    pub async fn ack_events(
        &self,
        session: &SessionRef,
        up_to_seq: u64,
    ) -> Result<SessionEventState, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let existing = read_state_row(&mut tx, session).await?;
        let (pruned, acked, committed) = existing.unwrap_or((0, 0, 0));
        if up_to_seq > committed {
            tx.rollback().await.map_err(db_error)?;
            return Err(StorageError::EventAckOutOfBounds {
                acked: up_to_seq,
                committed,
            });
        }
        if existing.is_none() {
            // `committed == 0`, so `up_to_seq == 0`: a pure no-op over the
            // all-zero state; never insert a row for a nonexistent session.
            tx.commit().await.map_err(db_error)?;
            return watermark_state(0, 0, 0);
        }
        let new_acked = acked.max(up_to_seq);
        sqlx::query(
            "UPDATE session_state SET acked_through_seq = ?3
              WHERE session_source = ?1 AND external_session_id = ?2",
        )
        .bind(session.source.as_str())
        .bind(session.external_id.as_str())
        .bind(i64_of(new_acked)?)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        watermark_state(pruned, new_acked, committed)
    }

    /// Prune the explicit acknowledged contiguous prefix `<= through_seq`:
    /// deletes exactly those `session_event` rows and advances
    /// `pruned_through_seq` to `through_seq` in the **same** transaction.
    ///
    /// A bound beyond `acked_through_seq` is refused with
    /// [`StorageError::EventPruneOutOfBounds`] and nothing is deleted.
    /// Neither `acked_through_seq` nor `last_committed_seq` is ever lowered;
    /// a repeated prune is harmless. Events never auto-expire. A missing
    /// session is a no-op over the all-zero state. Returns the resulting
    /// watermark state.
    pub async fn prune_events(
        &self,
        session: &SessionRef,
        through_seq: u64,
    ) -> Result<SessionEventState, StorageError> {
        let mut tx = self.pool().begin().await.map_err(db_error)?;
        let existing = read_state_row(&mut tx, session).await?;
        let (pruned, acked, committed) = existing.unwrap_or((0, 0, 0));
        if through_seq > acked {
            tx.rollback().await.map_err(db_error)?;
            return Err(StorageError::EventPruneOutOfBounds {
                through: through_seq,
                acked,
            });
        }
        if existing.is_none() {
            tx.commit().await.map_err(db_error)?;
            return watermark_state(0, 0, 0);
        }
        sqlx::query(
            "DELETE FROM session_event
              WHERE session_source = ?1 AND external_session_id = ?2
                AND event_seq <= ?3",
        )
        .bind(session.source.as_str())
        .bind(session.external_id.as_str())
        .bind(i64_of(through_seq)?)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        let new_pruned = pruned.max(through_seq);
        sqlx::query(
            "UPDATE session_state SET pruned_through_seq = ?3
              WHERE session_source = ?1 AND external_session_id = ?2",
        )
        .bind(session.source.as_str())
        .bind(session.external_id.as_str())
        .bind(i64_of(new_pruned)?)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        watermark_state(new_pruned, acked, committed)
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
        terminal: &TerminalRef,
        key: &KeyRow,
        exit: ExitResult,
    ) -> Result<Option<(RuntimeRecord, SessionEvent)>, StorageError> {
        let (kind, value) = exit_columns(exit)?;
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
        // Allocate the event on this same transaction: the process-state
        // flip and the event's sequence/watermark commit or roll back
        // together, so a state change can never exist without its event.
        let event = allocate_event(
            &mut tx,
            &terminal.session,
            &terminal.terminal_id,
            SessionEventPayload::ProcessExited(exit),
        )
        .await?;
        self.store.hit(CrashPoint::EventInsert);
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let record = row_to_record(&row)?;
        self.store.hit(CrashPoint::EventBeforeCommit);
        tx.commit().await.map_err(db_error)?;
        self.store.hit(CrashPoint::EventCommit);
        Ok(Some((record, event)))
    }

    async fn update_output(
        &self,
        terminal: &TerminalRef,
        key: &KeyRow,
        end: OutputEnd,
    ) -> Result<Option<(RuntimeRecord, SessionEvent)>, StorageError> {
        let end_text = output_end_text(end)?;
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
        .bind(end_text)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            tx.commit().await.map_err(db_error)?;
            return Ok(None);
        }
        // Same transaction as the output-state change (see
        // `update_with_exit`).
        let event = allocate_event(
            &mut tx,
            &terminal.session,
            &terminal.terminal_id,
            SessionEventPayload::OutputClosed(end),
        )
        .await?;
        self.store.hit(CrashPoint::EventInsert);
        let row = sqlx::query(SELECT_ONE)
            .bind(&key.0)
            .bind(&key.1)
            .bind(&key.2)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let record = row_to_record(&row)?;
        self.store.hit(CrashPoint::EventBeforeCommit);
        tx.commit().await.map_err(db_error)?;
        self.store.hit(CrashPoint::EventCommit);
        Ok(Some((record, event)))
    }

    async fn require(&self, key: &KeyRow) -> Result<RuntimeRecord, StorageError> {
        self.load_pool(key)
            .await?
            .ok_or_else(|| Self::conflict(format!("terminal {} has no runtime record", key.2)))
    }
}

const SELECT_ONE: &str = "SELECT * FROM terminal_runtime
     WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3";
