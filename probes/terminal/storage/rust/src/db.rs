//! Throwaway SQLite layer for probe C. NOT production code.
//!
//! Runtime queries only (`sqlx::query` + manual row extraction; no `query!`
//! macros, no ORM) and a runtime-loaded `Migrator::new` from the migrations
//! directory. Connections run WAL + `synchronous=FULL` + `foreign_keys` with a
//! busy timeout; the visible-index commit is a single short transaction that
//! contains no file scanning and no waiting on any client.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};

use crate::crash::CrashCtl;

/// Typed cursor rejection so callers can distinguish CURSOR_EXPIRED from
/// other failures. Carries the earliest available position as
/// (line, byte_offset within that line).
#[derive(Debug, Serialize)]
pub struct CursorExpired {
    pub earliest_line: u64,
    pub earliest_byte_offset: u64,
}

impl std::fmt::Display for CursorExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cursor expired; earliest available position line {} byte {}",
            self.earliest_line, self.earliest_byte_offset
        )
    }
}

impl std::error::Error for CursorExpired {}

#[derive(Debug, Clone)]
pub struct TerminalRow {
    pub terminal_id: String,
    pub terminal_uuid: [u8; 16],
    pub log_epoch: [u8; 16],
    pub line_watermark: u64,
    pub active_segment: Option<i64>,
    pub degraded: bool,
    pub process_status: String,
    pub output_status: String,
    pub exit_code: Option<i64>,
    pub exit_kind: Option<String>,
    /// Latched by drain overflow: a NEW writer start is refused (typed
    /// error) until a destructive rebuild clears it.
    pub refuse_new_start: bool,
}

#[derive(Debug, Clone)]
pub struct SegmentRow {
    pub segment_id: i64,
    pub terminal_id: String,
    pub file_name: String,
    pub first_line: u64,
    /// Exclusive end of the *available* line range; reduced (never enlarged)
    /// by recovery when committed data is missing or corrupt.
    pub last_line: u64,
    pub committed_bytes: u64,
    pub fsynced_bytes: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GapRow {
    pub first_line: u64,
    pub last_line: u64,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct TailRevisionRow {
    pub revision: u64,
    pub line: u64,
    /// Exact end-of-line byte offset of the fixed cursor.
    pub byte_offset: u64,
    pub segment_id: i64,
}

/// A tail revision published in the same transaction as its line: the
/// cursor is pinned at the exact end of that line.
#[derive(Debug, Clone)]
pub struct NewTailRevision {
    pub revision: u64,
    pub line: u64,
    pub byte_offset: u64,
    pub segment_id: i64,
}

pub struct NewSegment {
    pub terminal_id: String,
    pub file_name: String,
    pub first_line: u64,
    pub last_line: u64,
}

/// One short SQLite transaction publishing a committed file state. Ran only
/// after the appended frames have been `sync_data`d.
#[derive(Debug, Default)]
pub struct CommitInput {
    pub segment_id: i64,
    pub committed_bytes: u64,
    pub fsynced_bytes: u64,
    pub segment_last_line: u64,
    pub line_watermark: u64,
    pub tail: Option<NewTailRevision>,
    pub event: Option<(&'static str, String)>,
    pub exit: Option<(i64, &'static str)>,
}

pub struct Store {
    pool: SqlitePool,
    crash: CrashCtl,
}

fn row_terminal(row: &SqliteRow) -> Result<TerminalRow> {
    let uuid: Vec<u8> = row.try_get("terminal_uuid").context("terminal_uuid")?;
    let epoch: Vec<u8> = row.try_get("log_epoch").context("log_epoch")?;
    Ok(TerminalRow {
        terminal_id: row.try_get("terminal_id")?,
        terminal_uuid: uuid
            .as_slice()
            .try_into()
            .context("terminal_uuid must be 16 bytes")?,
        log_epoch: epoch
            .as_slice()
            .try_into()
            .context("log_epoch must be 16 bytes")?,
        line_watermark: u64::try_from(row.try_get::<i64, _>("line_watermark")?)?,
        active_segment: row.try_get("active_segment")?,
        degraded: row.try_get::<i64, _>("degraded")? != 0,
        process_status: row.try_get("process_status")?,
        output_status: row.try_get("output_status")?,
        exit_code: row.try_get("exit_code")?,
        exit_kind: row.try_get("exit_kind")?,
        refuse_new_start: row.try_get::<i64, _>("refuse_new_start")? != 0,
    })
}

fn row_segment(row: &SqliteRow) -> Result<SegmentRow> {
    Ok(SegmentRow {
        segment_id: row.try_get("segment_id")?,
        terminal_id: row.try_get("terminal_id")?,
        file_name: row.try_get("file_name")?,
        first_line: u64::try_from(row.try_get::<i64, _>("first_line")?)?,
        last_line: u64::try_from(row.try_get::<i64, _>("last_line")?)?,
        committed_bytes: u64::try_from(row.try_get::<i64, _>("committed_bytes")?)?,
        fsynced_bytes: u64::try_from(row.try_get::<i64, _>("fsynced_bytes")?)?,
        state: row.try_get("state")?,
    })
}

impl Store {
    /// Open (creating if needed) the scratch database, apply pragmas and run
    /// the runtime-loaded versioned migrations.
    pub async fn open(db_path: &Path, migrations_dir: &Path) -> Result<Store> {
        Store::open_with_crash(db_path, migrations_dir, CrashCtl::disabled()).await
    }

    /// `open` with a crash controller whose hooks fire inside the commit
    /// transaction (see `commit_visible`).
    pub async fn open_with_crash(
        db_path: &Path,
        migrations_dir: &Path,
        crash: CrashCtl,
    ) -> Result<Store> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("connect sqlite at {}", db_path.display()))?;
        let migrator = sqlx::migrate::Migrator::new(migrations_dir)
            .await
            .context("load migrations directory")?;
        migrator.run(&pool).await.context("run migrations")?;
        Ok(Store { pool, crash })
    }

    pub fn crash(&self) -> &CrashCtl {
        &self.crash
    }

    pub async fn close(self) {
        self.pool.close().await;
    }

    /// Test/fixture access to the pool (runtime queries only).
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn terminal_exists(&self, terminal_id: &str) -> Result<bool> {
        let row: Option<SqliteRow> =
            sqlx::query("SELECT 1 AS one FROM terminal WHERE terminal_id = ?1")
                .bind(terminal_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    pub async fn create_terminal(
        &self,
        terminal_id: &str,
        terminal_uuid: [u8; 16],
        epoch: [u8; 16],
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO terminal (terminal_id, terminal_uuid, log_epoch) VALUES (?1, ?2, ?3)",
        )
        .bind(terminal_id)
        .bind(terminal_uuid.as_slice())
        .bind(epoch.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_terminal(&self, terminal_id: &str) -> Result<TerminalRow> {
        let row = sqlx::query("SELECT * FROM terminal WHERE terminal_id = ?1")
            .bind(terminal_id)
            .fetch_optional(&self.pool)
            .await?
            .with_context(|| format!("terminal row {terminal_id} missing"))?;
        row_terminal(&row)
    }

    /// Destructive rebuild: new epoch, no active segment, fresh history. The
    /// line watermark is deliberately preserved so line numbers are never
    /// reused; old cursors fail validation against the new epoch.
    pub async fn destructive_rebuild(&self, terminal_id: &str, new_epoch: [u8; 16]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM tail_revision WHERE terminal_id = ?1")
            .bind(terminal_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM log_gap WHERE terminal_id = ?1")
            .bind(terminal_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM segment WHERE terminal_id = ?1")
            .bind(terminal_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE terminal
                SET log_epoch = ?2, active_segment = NULL, degraded = 0,
                    process_status = 'unknown', output_status = 'open',
                    exit_code = NULL, exit_kind = NULL, refuse_new_start = 0
              WHERE terminal_id = ?1",
        )
        .bind(terminal_id)
        .bind(new_epoch.as_slice())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn insert_segment(&self, seg: &NewSegment) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO segment (terminal_id, file_name, first_line, last_line)
             VALUES (?1, ?2, ?3, ?4)
             RETURNING segment_id",
        )
        .bind(&seg.terminal_id)
        .bind(&seg.file_name)
        .bind(i64::try_from(seg.first_line)?)
        .bind(i64::try_from(seg.last_line)?)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    pub async fn get_segment(&self, segment_id: i64) -> Result<SegmentRow> {
        let row = sqlx::query("SELECT * FROM segment WHERE segment_id = ?1")
            .bind(segment_id)
            .fetch_one(&self.pool)
            .await?;
        row_segment(&row)
    }

    pub async fn segment_by_file(&self, file_name: &str) -> Result<Option<SegmentRow>> {
        let row = sqlx::query("SELECT * FROM segment WHERE file_name = ?1")
            .bind(file_name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| row_segment(&r)).transpose()
    }

    pub async fn segments(&self, terminal_id: &str) -> Result<Vec<SegmentRow>> {
        let rows = sqlx::query("SELECT * FROM segment WHERE terminal_id = ?1 ORDER BY segment_id")
            .bind(terminal_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_segment).collect()
    }

    pub async fn seal_segment(&self, segment_id: i64) -> Result<()> {
        sqlx::query("UPDATE segment SET state = 'sealed' WHERE segment_id = ?1")
            .bind(segment_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Next segment id for file naming (ids are never reused thanks to
    /// AUTOINCREMENT, so quarantine artifacts cannot collide).
    pub async fn next_segment_id(&self) -> Result<i64> {
        let row: SqliteRow = sqlx::query("SELECT COALESCE(MAX(segment_id), 0) + 1 FROM segment")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>(0))
    }

    /// The one short visible-index transaction. Must run only after the file
    /// bytes it describes have been fsynced; contains no file I/O itself.
    /// `line_watermark` uses SQL `MAX` so it can never decrease, and the
    /// optional event (with exit status) commits in the same transaction.
    /// Crash hooks (txn_begin/update/event_insert/before+after commit) kill
    /// the process with `_exit(70)` at the exact statement boundaries; the
    /// assigned event sequence is returned for post-commit publication.
    pub async fn commit_visible(
        &self,
        terminal_id: &str,
        input: &CommitInput,
    ) -> Result<Option<u64>> {
        let mut tx = self.pool.begin().await?;
        self.crash.hit("txn_begin");
        sqlx::query(
            "UPDATE segment
                SET committed_bytes = ?1, fsynced_bytes = ?2, last_line = MAX(last_line, ?3)
              WHERE segment_id = ?4 AND terminal_id = ?5",
        )
        .bind(i64::try_from(input.committed_bytes)?)
        .bind(i64::try_from(input.fsynced_bytes)?)
        .bind(i64::try_from(input.segment_last_line)?)
        .bind(input.segment_id)
        .bind(terminal_id)
        .execute(&mut *tx)
        .await?;
        self.crash.hit("txn_update");
        match input.exit {
            Some((code, kind)) => {
                sqlx::query(
                    "UPDATE terminal
                        SET line_watermark = MAX(line_watermark, ?1), active_segment = ?2,
                            process_status = 'exited', output_status = 'closed',
                            exit_code = ?3, exit_kind = ?4
                      WHERE terminal_id = ?5",
                )
                .bind(i64::try_from(input.line_watermark)?)
                .bind(input.segment_id)
                .bind(code)
                .bind(kind)
                .bind(terminal_id)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE terminal
                        SET line_watermark = MAX(line_watermark, ?1), active_segment = ?2
                      WHERE terminal_id = ?3",
                )
                .bind(i64::try_from(input.line_watermark)?)
                .bind(input.segment_id)
                .bind(terminal_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        if let Some(tail) = &input.tail {
            sqlx::query(
                "INSERT INTO tail_revision (terminal_id, revision, line, byte_offset, segment_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (terminal_id, revision) DO UPDATE
                    SET line = excluded.line, byte_offset = excluded.byte_offset,
                        segment_id = excluded.segment_id",
            )
            .bind(terminal_id)
            .bind(i64::try_from(tail.revision)?)
            .bind(i64::try_from(tail.line)?)
            .bind(i64::try_from(tail.byte_offset)?)
            .bind(tail.segment_id)
            .execute(&mut *tx)
            .await?;
        }
        let mut event_seq: Option<u64> = None;
        if let Some((kind, payload)) = &input.event {
            sqlx::query("INSERT INTO session (session_id) VALUES (?1) ON CONFLICT DO NOTHING")
                .bind(terminal_id)
                .execute(&mut *tx)
                .await?;
            // The next sequence comes from the locked session_state counter,
            // never from retained event rows: after a full prune the event
            // table is empty and MAX(event_seq) would restart at 1 and reuse
            // an already-published sequence.
            let next: i64 = sqlx::query(
                "SELECT COALESCE((SELECT last_appended_seq FROM session_state \
                   WHERE session_id = ?1), 0) + 1",
            )
            .bind(terminal_id)
            .fetch_one(&mut *tx)
            .await?
            .get(0);
            sqlx::query(
                "INSERT INTO session_event (session_id, event_seq, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(terminal_id)
            .bind(next)
            .bind(kind)
            .bind(payload)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO session_state (session_id, last_appended_seq) VALUES (?1, ?2)
                 ON CONFLICT (session_id) DO UPDATE SET last_appended_seq = excluded.last_appended_seq",
            )
            .bind(terminal_id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
            event_seq = Some(u64::try_from(next)?);
        }
        self.crash.hit("event_insert");
        self.crash.hit("txn_before_commit");
        tx.commit().await?;
        self.crash.hit("txn_after_commit");
        self.crash.hit("event_commit");
        Ok(event_seq)
    }

    /// Ack is monotonic and bounded by the committed event sequence
    /// (`acked <= last_appended`); over-bound acks are rejected.
    pub async fn ack_events(&self, session_id: &str, acked_through: u64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let last: i64 =
            sqlx::query("SELECT last_appended_seq FROM session_state WHERE session_id = ?1")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|r| r.get::<i64, _>(0))
                .unwrap_or(0);
        if i64::try_from(acked_through)? > last {
            bail!("ack {acked_through} exceeds committed event bound {last}");
        }
        sqlx::query(
            "UPDATE session_state SET acked_through_seq = MAX(acked_through_seq, ?1) WHERE session_id = ?2",
        )
        .bind(i64::try_from(acked_through)?)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Prune deletes only the acked continuous prefix and updates
    /// `pruned_through_seq` in the same transaction.
    pub async fn prune_events(&self, session_id: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let acked: i64 =
            sqlx::query("SELECT acked_through_seq FROM session_state WHERE session_id = ?1")
                .bind(session_id)
                .fetch_one(&mut *tx)
                .await?
                .get(0);
        sqlx::query("DELETE FROM session_event WHERE session_id = ?1 AND event_seq <= ?2")
            .bind(session_id)
            .bind(acked)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE session_state SET pruned_through_seq = MAX(pruned_through_seq, ?1) WHERE session_id = ?2",
        )
        .bind(acked)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(u64::try_from(acked)?)
    }

    pub async fn event_count(&self, session_id: &str) -> Result<u64> {
        let n: i64 = sqlx::query("SELECT COUNT(*) FROM session_event WHERE session_id = ?1")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?
            .get(0);
        Ok(u64::try_from(n)?)
    }

    pub async fn session_state(&self, session_id: &str) -> Result<Option<(u64, u64, u64)>> {
        let row = sqlx::query(
            "SELECT pruned_through_seq, acked_through_seq, last_appended_seq
               FROM session_state WHERE session_id = ?1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                u64::try_from(r.get::<i64, _>(0)).unwrap(),
                u64::try_from(r.get::<i64, _>(1)).unwrap(),
                u64::try_from(r.get::<i64, _>(2)).unwrap(),
            )
        }))
    }

    /// Idempotent explicit gap record for a missing line range (exclusive end).
    pub async fn add_log_gap(
        &self,
        terminal_id: &str,
        first_line: u64,
        last_line: u64,
        reason: &str,
    ) -> Result<()> {
        if first_line >= last_line {
            return Ok(()); // empty range: nothing missing
        }
        let created_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )?;
        sqlx::query(
            "INSERT OR IGNORE INTO log_gap (terminal_id, first_line, last_line, reason, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(terminal_id)
        .bind(i64::try_from(first_line)?)
        .bind(i64::try_from(last_line)?)
        .bind(reason)
        .bind(i64::try_from(created_ms)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_gaps(&self, terminal_id: &str) -> Result<Vec<GapRow>> {
        let rows = sqlx::query(
            "SELECT first_line, last_line, reason FROM log_gap WHERE terminal_id = ?1 ORDER BY first_line",
        )
        .bind(terminal_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| GapRow {
                first_line: u64::try_from(r.get::<i64, _>(0)).unwrap(),
                last_line: u64::try_from(r.get::<i64, _>(1)).unwrap(),
                reason: r.get::<String, _>(2),
            })
            .collect())
    }

    /// Resolve one tail revision to its fixed cursor. `None` when the
    /// revision is unknown or was expired by resource recovery.
    pub async fn tail_revision(
        &self,
        terminal_id: &str,
        revision: u64,
    ) -> Result<Option<TailRevisionRow>> {
        let row = sqlx::query(
            "SELECT revision, line, byte_offset, segment_id \
               FROM tail_revision WHERE terminal_id = ?1 AND revision = ?2",
        )
        .bind(terminal_id)
        .bind(i64::try_from(revision)?)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| TailRevisionRow {
            revision: u64::try_from(r.get::<i64, _>(0)).unwrap(),
            line: u64::try_from(r.get::<i64, _>(1)).unwrap(),
            byte_offset: u64::try_from(r.get::<i64, _>(2)).unwrap(),
            segment_id: r.get::<i64, _>(3),
        }))
    }

    /// The newest persisted tail revision (revision addressing is
    /// monotonic; lines are never reused).
    pub async fn latest_tail_revision(&self, terminal_id: &str) -> Result<Option<TailRevisionRow>> {
        let row = sqlx::query(
            "SELECT revision, line, byte_offset, segment_id \
               FROM tail_revision WHERE terminal_id = ?1 \
               ORDER BY revision DESC LIMIT 1",
        )
        .bind(terminal_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| TailRevisionRow {
            revision: u64::try_from(r.get::<i64, _>(0)).unwrap(),
            line: u64::try_from(r.get::<i64, _>(1)).unwrap(),
            byte_offset: u64::try_from(r.get::<i64, _>(2)).unwrap(),
            segment_id: r.get::<i64, _>(3),
        }))
    }

    /// Gate introspection: applied migration versions in order.
    pub async fn applied_migration_versions(&self) -> Result<Vec<i64>> {
        let rows = sqlx::query("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get::<i64, _>(0)).collect())
    }

    /// Gate introspection: does a table exist (migration rollback checks)?
    pub async fn table_exists(&self, name: &str) -> Result<bool> {
        let row: Option<SqliteRow> =
            sqlx::query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    /// Gate introspection: session events (seq, kind) in order.
    pub async fn session_events(&self, session_id: &str) -> Result<Vec<(u64, String)>> {
        let rows = sqlx::query(
            "SELECT event_seq, kind FROM session_event WHERE session_id = ?1 ORDER BY event_seq",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    u64::try_from(r.get::<i64, _>(0)).unwrap(),
                    r.get::<String, _>(1),
                )
            })
            .collect())
    }

    /// Latch degraded state (one-way within an epoch).
    pub async fn set_degraded(&self, terminal_id: &str) -> Result<()> {
        sqlx::query("UPDATE terminal SET degraded = 1 WHERE terminal_id = ?1")
            .bind(terminal_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Latch the drain-overflow refusal (operator reset = destructive
    /// rebuild). Latching is idempotent.
    pub async fn set_refuse_new_start(&self, terminal_id: &str) -> Result<()> {
        sqlx::query("UPDATE terminal SET refuse_new_start = 1 WHERE terminal_id = ?1")
            .bind(terminal_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resource recovery: delete every sealed segment strictly older than
    /// `keep_segment_id` (the tail/active segment is always kept) and expire
    /// the tail revisions that pointed into them, in ONE transaction. The
    /// caller removes the files after the commit and fsyncs the directory.
    /// Returns the deleted rows (oldest first).
    pub async fn cleanup_old_segments(
        &self,
        terminal_id: &str,
        keep_segment_id: i64,
    ) -> Result<Vec<SegmentRow>> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT * FROM segment
              WHERE terminal_id = ?1 AND segment_id < ?2 AND state = 'sealed'
              ORDER BY segment_id",
        )
        .bind(terminal_id)
        .bind(keep_segment_id)
        .fetch_all(&mut *tx)
        .await?;
        let deleted: Vec<SegmentRow> = rows.iter().map(row_segment).collect::<Result<_>>()?;
        // Expire the tail revisions that point into the deleted range FIRST
        // (their segments are about to vanish; the FK requires this order).
        sqlx::query("DELETE FROM tail_revision WHERE terminal_id = ?1 AND segment_id < ?2")
            .bind(terminal_id)
            .bind(keep_segment_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM segment
              WHERE terminal_id = ?1 AND segment_id < ?2 AND state = 'sealed'",
        )
        .bind(terminal_id)
        .bind(keep_segment_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(deleted)
    }

    /// Post-repair segment state: reduced committed/fsynced bytes and the
    /// reduced available range. Unlike `commit_visible` this may lower
    /// `last_line` (that is the point of recovery), never `line_watermark`.
    pub async fn repair_segment(
        &self,
        segment_id: i64,
        committed_bytes: u64,
        fsynced_bytes: u64,
        last_line: u64,
        state: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE segment
                SET committed_bytes = ?1, fsynced_bytes = ?2, last_line = ?3, state = ?4
              WHERE segment_id = ?5",
        )
        .bind(i64::try_from(committed_bytes)?)
        .bind(i64::try_from(fsynced_bytes)?)
        .bind(i64::try_from(last_line)?)
        .bind(state)
        .bind(segment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl TerminalRow {
    /// Validate a (epoch, line) cursor: a different log epoch (destructive
    /// rebuild) or a line below the earliest available position is expired;
    /// the earliest available (line, byte_offset) is returned with the
    /// rejection. Earliest positions are always line starts, so only the
    /// line participates in the comparison.
    pub fn validate_cursor(
        &self,
        cursor_epoch: &[u8; 16],
        cursor_line: u64,
        earliest_available: (u64, u64),
    ) -> std::result::Result<(), CursorExpired> {
        if cursor_epoch != &self.log_epoch || cursor_line < earliest_available.0 {
            return Err(CursorExpired {
                earliest_line: earliest_available.0,
                earliest_byte_offset: earliest_available.1,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ql-storage-db-{}-{}",
            tag,
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn migrations() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
    }

    async fn store(tag: &str) -> (PathBuf, Store) {
        let root = test_root(tag);
        let s = Store::open(&root.join("terminal.db"), &migrations())
            .await
            .unwrap();
        (root, s)
    }

    #[tokio::test]
    async fn migration_creates_schema_and_replays_idempotently() {
        let (root, s) = store("migrate").await;
        // Reopening runs the same migrations again (already applied).
        let s2 = Store::open(&root.join("terminal.db"), &migrations())
            .await
            .unwrap();
        s2.close().await;
        s.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn watermark_never_decreases_via_commit() {
        let (_root, s) = store("watermark").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let seg = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        let commit = |watermark: u64| CommitInput {
            segment_id: seg,
            committed_bytes: 128,
            fsynced_bytes: 128,
            segment_last_line: watermark + 1,
            line_watermark: watermark,
            ..Default::default()
        };
        s.commit_visible("t", &commit(5)).await.unwrap();
        s.commit_visible("t", &commit(3)).await.unwrap(); // stale/lower value
        let term = s.get_terminal("t").await.unwrap();
        assert_eq!(term.line_watermark, 5);
        s.close().await;
    }

    #[tokio::test]
    async fn event_sequence_and_bounds() {
        let (_root, s) = store("events").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let seg = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        for i in 1..=3u64 {
            s.commit_visible(
                "t",
                &CommitInput {
                    segment_id: seg,
                    committed_bytes: 64 * i,
                    fsynced_bytes: 64 * i,
                    segment_last_line: i + 1,
                    line_watermark: i,
                    event: Some(("output", format!("{{\"line\":{i}}}"))),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let (pruned, acked, last) = s.session_state("t").await.unwrap().unwrap();
        assert_eq!((pruned, acked, last), (0, 0, 3));
        assert_eq!(s.event_count("t").await.unwrap(), 3);

        // over-bound ack rejected
        assert!(s.ack_events("t", 4).await.is_err());
        s.ack_events("t", 2).await.unwrap();
        // monotonic: older ack does not move it back
        s.ack_events("t", 1).await.unwrap();
        assert_eq!(s.session_state("t").await.unwrap().unwrap().1, 2);
        // prune removes only the acked prefix
        let pruned_to = s.prune_events("t").await.unwrap();
        assert_eq!(pruned_to, 2);
        assert_eq!(s.event_count("t").await.unwrap(), 1);
        let (pruned, acked, last) = s.session_state("t").await.unwrap().unwrap();
        assert_eq!((pruned, acked, last), (2, 2, 3));
        s.close().await;
    }

    #[tokio::test]
    async fn event_sequence_continues_after_full_prune() {
        let (_root, s) = store("prune-replay").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let seg = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        let commit = |watermark: u64| CommitInput {
            segment_id: seg,
            committed_bytes: 64 * watermark,
            fsynced_bytes: 64 * watermark,
            segment_last_line: watermark + 1,
            line_watermark: watermark,
            event: Some(("output", format!("{{\"line\":{watermark}}}"))),
            ..Default::default()
        };
        for i in 1..=3u64 {
            s.commit_visible("t", &commit(i)).await.unwrap();
        }
        // Ack and prune everything: the event table is now empty, so a
        // MAX(event_seq)-based allocator would restart at 1 and reuse a
        // published sequence. The counter must come from session_state.
        s.ack_events("t", 3).await.unwrap();
        s.prune_events("t").await.unwrap();
        assert_eq!(s.event_count("t").await.unwrap(), 0);
        let seq = s
            .commit_visible("t", &commit(4))
            .await
            .unwrap()
            .expect("event seq assigned");
        assert_eq!(seq, 4, "sequence must continue from last_appended_seq");
        let (pruned, acked, last) = s.session_state("t").await.unwrap().unwrap();
        assert_eq!((pruned, acked, last), (3, 3, 4));
        let events = s.session_events("t").await.unwrap();
        assert_eq!(events, vec![(4u64, "output".to_string())]);
        s.close().await;
    }

    #[tokio::test]
    async fn tail_revisions_persist_and_expire_with_cleanup() {
        let (_root, s) = store("tail-revisions").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let seg1 = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        let seg2 = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000002.log".into(),
                first_line: 3,
                last_line: 3,
            })
            .await
            .unwrap();
        s.commit_visible(
            "t",
            &CommitInput {
                segment_id: seg1,
                committed_bytes: 200,
                fsynced_bytes: 200,
                segment_last_line: 3,
                line_watermark: 2,
                tail: Some(NewTailRevision {
                    revision: 2,
                    line: 2,
                    byte_offset: 120,
                    segment_id: seg1,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        s.commit_visible(
            "t",
            &CommitInput {
                segment_id: seg2,
                committed_bytes: 200,
                fsynced_bytes: 200,
                segment_last_line: 4,
                line_watermark: 3,
                tail: Some(NewTailRevision {
                    revision: 3,
                    line: 3,
                    byte_offset: 120,
                    segment_id: seg2,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Every revision stays resolvable (history is never overwritten).
        let r2 = s.tail_revision("t", 2).await.unwrap().unwrap();
        assert_eq!((r2.revision, r2.line, r2.byte_offset), (2, 2, 120));
        let latest = s.latest_tail_revision("t").await.unwrap().unwrap();
        assert_eq!(latest.revision, 3);
        assert!(s.tail_revision("t", 99).await.unwrap().is_none());

        // Reclaiming the (sealed) seg1 expires exactly its revisions,
        // transactionally.
        s.seal_segment(seg1).await.unwrap();
        let deleted = s.cleanup_old_segments("t", seg2).await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert!(s.tail_revision("t", 2).await.unwrap().is_none());
        let latest = s.latest_tail_revision("t").await.unwrap().unwrap();
        assert_eq!(latest.revision, 3);
        s.close().await;
    }

    #[tokio::test]
    async fn composite_event_key_rejects_duplicates() {
        let (_root, s) = store("composite-key").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        s.commit_visible(
            "t",
            &CommitInput {
                segment_id: s
                    .insert_segment(&NewSegment {
                        terminal_id: "t".into(),
                        file_name: "seg-000001.log".into(),
                        first_line: 1,
                        last_line: 1,
                    })
                    .await
                    .unwrap(),
                committed_bytes: 10,
                fsynced_bytes: 10,
                segment_last_line: 2,
                line_watermark: 1,
                event: Some(("output", "p".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // same (session, seq) must be impossible to insert twice
        let dup = sqlx::query("INSERT INTO session_event (session_id, event_seq, kind, payload) VALUES ('t', 1, 'x', 'y')")
            .execute(&s.pool)
            .await;
        assert!(dup.is_err());
        s.close().await;
    }

    #[tokio::test]
    async fn state_check_rejects_bad_ordering() {
        let (_root, s) = store("check-constraint").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let bad = sqlx::query(
            "INSERT INTO session (session_id) VALUES ('t');
             INSERT INTO session_state (session_id, pruned_through_seq, acked_through_seq, last_appended_seq)
             VALUES ('t', 2, 1, 1)",
        )
        .execute(&s.pool)
        .await;
        assert!(bad.is_err()); // pruned <= acked <= last violated
        s.close().await;
    }

    #[tokio::test]
    async fn destructive_rebuild_rotates_epoch_keeps_watermark() {
        let (_root, s) = store("rebuild").await;
        s.create_terminal("t", [1; 16], [2; 16]).await.unwrap();
        let seg = s
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        s.commit_visible(
            "t",
            &CommitInput {
                segment_id: seg,
                committed_bytes: 100,
                fsynced_bytes: 100,
                segment_last_line: 6,
                line_watermark: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        s.add_log_gap("t", 2, 4, "missing").await.unwrap();
        let new_epoch = uuid::Uuid::now_v7().into_bytes();
        s.destructive_rebuild("t", new_epoch).await.unwrap();
        let term = s.get_terminal("t").await.unwrap();
        assert_eq!(term.log_epoch, new_epoch);
        assert_eq!(term.line_watermark, 5); // preserved
        assert_eq!(term.active_segment, None);
        assert!(s.segments("t").await.unwrap().is_empty());
        assert!(s.list_gaps("t").await.unwrap().is_empty());
        // old-epoch cursor now rejected with earliest position
        let err = term.validate_cursor(&[2; 16], 1, (1, 0)).unwrap_err();
        assert_eq!(err.earliest_line, 1);
        assert_eq!(err.earliest_byte_offset, 0);
        s.close().await;
    }
}
