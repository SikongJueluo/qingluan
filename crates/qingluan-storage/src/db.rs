//! Internal SQLite layer: connection setup, migrations, and every runtime
//! query of the terminal log.
//!
//! Runtime queries only (`sqlx::query` + manual row extraction; no `query!`
//! macros, no ORM, no cross-database traits). Connections run WAL +
//! `synchronous=FULL` + `foreign_keys` with a busy timeout on a
//! single-connection pool. The visible-index commit is one short transaction
//! that contains no file scanning and never waits on a client.
//!
//! Recovery and retention transactions share the same discipline. Every
//! durable repair is idempotent on its own: gap recording rewrites one
//! stream's coalesced gap set, segment deletion clears any active pointer
//! that references the deleted row, and the retention reclaim advances the
//! retained floor in the same transaction that deletes the reclaimed row,
//! so a crash between any two steps converges on the next pass instead of
//! fabricating or losing bookkeeping.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteQueryResult, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{Row, SqlSafeStr, SqlitePool};

use crate::LogStream;
use crate::crash::{CrashPoint, CrashSink};
use crate::error::{StorageError, db_error, i64_of, io_error, migrate_error, u64_of};
use crate::gap::{GapReason, GapSpan, coalesce};
use crate::identity::{HeaderIdentity, LogKey};
use crate::paths;

/// Persisted on-disk format version this build reads and writes.
pub(crate) const FORMAT_VERSION: &str = "4";

/// One terminal row. The two stream pointers, watermarks, and retained
/// floors are independent: `active_normalized_segment`/`line_watermark`/
/// `retained_first_line` track the normalized line stream,
/// `active_raw_segment`/`raw_watermark`/`retained_first_offset` track the
/// raw byte stream.
#[derive(Debug, Clone)]
pub(crate) struct TerminalRow {
    pub terminal_uuid: [u8; 16],
    pub log_epoch: [u8; 16],
    pub line_watermark: u64,
    pub raw_watermark: u64,
    // Written by retention reclamation and recovery's gap latch; read by
    // the query surface (the retained window) and by the test-hooks
    // snapshot.
    pub retained_first_line: u64,
    pub retained_first_offset: u64,
    pub active_normalized_segment: Option<i64>,
    pub active_raw_segment: Option<i64>,
    // Latched (never cleared) by recovery gaps and unsafe reclaims;
    // reported on every query page, deliberately not a write blocker.
    pub degraded: bool,
    pub refuse_new_start: bool,
}

/// One segment row of exactly one stream, with its exclusive range columns
/// (`first_line`/`last_line` for normalized, `first_offset`/`last_offset`
/// for raw) as written by the insert and visibility transactions.
#[derive(Debug, Clone)]
pub(crate) struct SegmentRow {
    pub segment_id: i64,
    pub kind: LogStream,
    pub file_name: String,
    pub committed_bytes: u64,
    pub fsynced_bytes: u64,
    pub state: String,
    pub first_line: Option<u64>,
    pub last_line: Option<u64>,
    pub first_offset: u64,
    pub last_offset: u64,
}

impl SegmentRow {
    /// The exclusive range of this row in its stream's coordinates
    /// (normalized: `[first_line, last_line)`; raw:
    /// `[first_offset, last_offset)`), or `None` when nothing was ever
    /// committed into it (empty range).
    pub(crate) fn stream_range(&self) -> Option<(u64, u64)> {
        match self.kind {
            LogStream::Normalized => match (self.first_line, self.last_line) {
                (Some(first), Some(last)) if last > first => Some((first, last)),
                _ => None,
            },
            LogStream::Raw => {
                if self.last_offset > self.first_offset {
                    Some((self.first_offset, self.last_offset))
                } else {
                    None
                }
            }
        }
    }
}

/// The stream-scoped half of one visibility transaction: the exclusive
/// range end inside the receiving segment plus the new terminal
/// watermark of that stream, and — as a compare-and-set — the durable
/// values this writer last published for both. The CAS is the commit's
/// ownership guard: the transaction refuses (typed
/// [`StorageError::CommitConflict`]) when the watermark or the
/// segment's indexed range moved since this writer cached it, so a
/// second writer (or external database modification) can never commit
/// overlapping ranges behind a monotonic `MAX` update.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StreamCommit {
    /// Normalized stream: exclusive last line and new line watermark.
    Normalized {
        segment_last_line: u64,
        line_watermark: u64,
        /// The receiving segment row's indexed `last_line` this commit
        /// continues from (the exclusive end of the previous batch).
        prior_segment_last_line: u64,
        /// The terminal's `line_watermark` before this batch (the first
        /// line of the batch minus one).
        prior_line_watermark: u64,
    },
    /// Raw stream: exclusive last byte offset and new byte watermark.
    Raw {
        segment_last_offset: u64,
        raw_watermark: u64,
        /// The receiving segment row's indexed `last_offset` this commit
        /// continues from.
        prior_segment_last_offset: u64,
        /// The terminal's `raw_watermark` before this batch (the stream
        /// offset this batch starts at).
        prior_raw_watermark: u64,
    },
}

/// One short SQLite transaction publishing a committed file state. Runs
/// only after the appended frames have been `sync_data`d. The stream is
/// identified by the [`StreamCommit`] variant, which also carries the
/// stream's new range end and watermark. Extensible for later stages
/// (event/exit slots) without widening the public seam.
#[derive(Debug)]
pub(crate) struct CommitInput {
    pub segment_id: i64,
    pub committed_bytes: u64,
    pub fsynced_bytes: u64,
    pub commit: StreamCommit,
}

/// Result of one segment creation transaction: the fresh row's id and file
/// name plus the file of the segment reclaimed to keep the per-terminal
/// metadata budget (the row is already gone; the caller unlinks the file
/// after the transaction committed).
#[derive(Debug)]
pub(crate) struct InsertedSegment {
    pub segment_id: i64,
    pub file_name: String,
    pub reclaimed_file_name: Option<String>,
}

/// Outcome of the segment-creation attempt itself.
#[derive(Debug)]
pub(crate) enum SegmentCreation {
    /// The row exists and nothing was written to its file yet.
    Created(InsertedSegment),
    /// No sealed segment could be reclaimed safely within the metadata
    /// budget: the transaction changed nothing and no row exists. The
    /// caller answers by dropping the batch it wanted to persist:
    /// [`Store::drop_batch`] records the stream-scoped gap, advances the
    /// non-reusable watermark, and latches `degraded` +
    /// `refuse_new_start` in one transaction. Only future terminal starts
    /// are refused — the already-running writer keeps draining (each
    /// further append records its own drop).
    UnsafeReclaim,
}

pub(crate) struct Store {
    pool: SqlitePool,
    crash: Mutex<Option<CrashSink>>,
}

fn embedded_migrator() -> Migrator {
    Migrator::with_migrations(vec![
        Migration::new(
            1,
            "terminal_log".into(),
            MigrationType::Simple,
            include_str!("../migrations/0001_terminal_log.sql").into_sql_str(),
            false,
        ),
        Migration::new(
            2,
            "recovery_retention".into(),
            MigrationType::Simple,
            include_str!("../migrations/0002_recovery_retention.sql").into_sql_str(),
            false,
        ),
        Migration::new(
            3,
            "terminal_runtime".into(),
            MigrationType::Simple,
            include_str!("../migrations/0003_terminal_runtime.sql").into_sql_str(),
            false,
        ),
        Migration::new(
            4,
            "session_events".into(),
            MigrationType::Simple,
            include_str!("../migrations/0004_session_events.sql").into_sql_str(),
            false,
        ),
    ])
}

fn row_terminal(row: &SqliteRow) -> Result<TerminalRow, StorageError> {
    let terminal_uuid: Vec<u8> = row.try_get("terminal_uuid").map_err(db_error)?;
    let epoch: Vec<u8> = row.try_get("log_epoch").map_err(db_error)?;
    Ok(TerminalRow {
        terminal_uuid: terminal_uuid
            .as_slice()
            .try_into()
            .map_err(|_| StorageError::Database("terminal_uuid must be 16 bytes".into()))?,
        log_epoch: epoch
            .as_slice()
            .try_into()
            .map_err(|_| StorageError::Database("log_epoch must be 16 bytes".into()))?,
        line_watermark: u64_of(row.try_get::<i64, _>("line_watermark").map_err(db_error)?)?,
        raw_watermark: u64_of(row.try_get::<i64, _>("raw_watermark").map_err(db_error)?)?,
        retained_first_line: u64_of(
            row.try_get::<i64, _>("retained_first_line")
                .map_err(db_error)?,
        )?,
        retained_first_offset: u64_of(
            row.try_get::<i64, _>("retained_first_offset")
                .map_err(db_error)?,
        )?,
        active_normalized_segment: row.try_get("active_normalized_segment").map_err(db_error)?,
        active_raw_segment: row.try_get("active_raw_segment").map_err(db_error)?,
        degraded: row.try_get::<i64, _>("degraded").map_err(db_error)? != 0,
        refuse_new_start: row
            .try_get::<i64, _>("refuse_new_start")
            .map_err(db_error)?
            != 0,
    })
}

fn row_segment(row: &SqliteRow) -> Result<SegmentRow, StorageError> {
    let kind: String = row.try_get("kind").map_err(db_error)?;
    let kind = LogStream::from_db_text(&kind)
        .ok_or_else(|| StorageError::Database(format!("unknown segment kind {kind:?}")))?;
    let first_line = match row
        .try_get::<Option<i64>, _>("first_line")
        .map_err(db_error)?
    {
        Some(line) => Some(u64_of(line)?),
        None => None,
    };
    let last_line = match row
        .try_get::<Option<i64>, _>("last_line")
        .map_err(db_error)?
    {
        Some(line) => Some(u64_of(line)?),
        None => None,
    };
    Ok(SegmentRow {
        segment_id: row.try_get("segment_id").map_err(db_error)?,
        kind,
        file_name: row.try_get("file_name").map_err(db_error)?,
        committed_bytes: u64_of(row.try_get::<i64, _>("committed_bytes").map_err(db_error)?)?,
        fsynced_bytes: u64_of(row.try_get::<i64, _>("fsynced_bytes").map_err(db_error)?)?,
        state: row.try_get("state").map_err(db_error)?,
        first_line,
        last_line,
        first_offset: u64_of(row.try_get::<i64, _>("first_offset").map_err(db_error)?)?,
        last_offset: u64_of(row.try_get::<i64, _>("last_offset").map_err(db_error)?)?,
    })
}

impl Store {
    /// Open (creating if needed) the database at `root`, apply pragmas, run
    /// the versioned migrations, and verify the persisted format version.
    pub(crate) async fn open(root: &Path) -> Result<Arc<Store>, StorageError> {
        Store::open_with_migrator(root, embedded_migrator()).await
    }

    /// The underlying pool, for sibling modules that own their own narrow
    /// tables on the same storage root (the S3 runtime registry).
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// [`Store::open`] with an explicit migrator (runtime-loaded migration
    /// directory in tests).
    pub(crate) async fn open_with_migrator(
        root: &Path,
        migrator: Migrator,
    ) -> Result<Arc<Store>, StorageError> {
        tokio::fs::create_dir_all(root)
            .await
            .map_err(io_error(root))?;
        let db_path = paths::db_path(root);
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|error| {
                StorageError::Database(format!("connect sqlite at {}: {error}", db_path.display()))
            })?;
        let store = Arc::new(Store {
            pool,
            crash: Mutex::new(None),
        });
        migrator.run(&store.pool).await.map_err(migrate_error)?;
        store.check_format_version().await?;
        Ok(store)
    }

    async fn check_format_version(&self) -> Result<(), StorageError> {
        let found: Option<(String,)> =
            sqlx::query_as("SELECT value FROM meta WHERE key = 'format_version'")
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?;
        match found {
            Some((version,)) if version == FORMAT_VERSION => Ok(()),
            Some((version,)) => Err(StorageError::FormatVersionUnsupported {
                found: version,
                supported: FORMAT_VERSION.to_owned(),
            }),
            None => Err(StorageError::Database(
                "meta.format_version missing after migration".into(),
            )),
        }
    }

    /// Fire a failpoint site; inert unless a test sink is installed.
    pub(crate) fn hit(&self, point: CrashPoint) {
        let sink = self.crash.lock().expect("crash sink lock");
        if let Some(sink) = sink.as_ref() {
            sink(point);
        }
    }

    /// Install a failpoint observer (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn set_crash_sink(&self, sink: Option<CrashSink>) {
        *self.crash.lock().expect("crash sink lock") = sink;
    }

    pub(crate) async fn terminal(&self, key: &LogKey) -> Result<Option<TerminalRow>, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM terminal
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        row.map(|row| row_terminal(&row)).transpose()
    }

    /// Fetch the terminal row, creating it (with the given identity) when
    /// absent. Returns the row and whether this call created it.
    pub(crate) async fn ensure_terminal(
        &self,
        key: &LogKey,
        identity: HeaderIdentity,
    ) -> Result<(TerminalRow, bool), StorageError> {
        if let Some(row) = self.terminal(key).await? {
            return Ok((row, false));
        }
        sqlx::query(
            "INSERT INTO terminal
                 (session_source, external_session_id, terminal_id, terminal_uuid, log_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO NOTHING",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(identity.terminal_uuid.as_slice())
        .bind(identity.epoch.as_slice())
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok((
            self.terminal(key)
                .await?
                .expect("terminal row just created"),
            true,
        ))
    }

    /// Insert a segment row of exactly one stream, deriving its file name
    /// from the returned id inside one transaction (ids are never reused;
    /// the name can never collide with a quarantine artifact). Normalized
    /// segments anchor at `first_line`; raw segments anchor at the stream
    /// byte offset `first_offset` and carry no line numbers.
    ///
    /// The per-terminal metadata budget applies across both streams
    /// combined: before the creation that would exceed
    /// [`crate::MAX_SEGMENT_METADATA_ROWS`] live rows, the same transaction
    /// reclaims the oldest sealed segment (never one referenced by an
    /// active pointer) and advances the retained floor of its stream to the
    /// first position still backed by a surviving row. Tail-line mappings
    /// that anchored inside a reclaimed range join this transaction once
    /// the S4 revision surface exists; there is nothing to remove yet.
    /// When no sealed segment can be reclaimed safely the creation is
    /// refused without any mutation ([`SegmentCreation::UnsafeReclaim`]);
    /// the caller records the dropped batch via [`Store::drop_batch`],
    /// which is what latches `degraded` + `refuse_new_start`.
    pub(crate) async fn insert_segment(
        &self,
        key: &LogKey,
        stream: LogStream,
        first_line: Option<u64>,
        first_offset: u64,
    ) -> Result<SegmentCreation, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let term = Self::terminal_tx(&mut tx, key).await?;
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM segment
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND state IN ('active', 'sealed')",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let budget = i64::try_from(crate::MAX_SEGMENT_METADATA_ROWS)
            .expect("segment metadata budget fits i64");
        let mut reclaimed_file_name = None;
        if live >= budget {
            let candidates = sqlx::query(
                "SELECT * FROM segment
                  WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                    AND state = 'sealed'
                  ORDER BY segment_id",
            )
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
            let oldest = candidates
                .iter()
                .map(row_segment)
                .collect::<Result<Vec<SegmentRow>, StorageError>>()?
                .into_iter()
                .find(|row| {
                    Some(row.segment_id) != term.active_normalized_segment
                        && Some(row.segment_id) != term.active_raw_segment
                });
            match oldest {
                Some(row) => {
                    sqlx::query("DELETE FROM segment WHERE segment_id = ?1")
                        .bind(row.segment_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_error)?;
                    match row.kind {
                        LogStream::Normalized => {
                            // The exclusive end of the reclaimed range is the
                            // first line still backed by a surviving row.
                            let floor = row.last_line.unwrap_or(row.first_line.unwrap_or(1));
                            sqlx::query(
                                "UPDATE terminal
                                    SET retained_first_line = MAX(retained_first_line, ?1)
                                  WHERE session_source = ?2 AND external_session_id = ?3
                                    AND terminal_id = ?4",
                            )
                            .bind(i64_of(floor)?)
                            .bind(&key.session_source)
                            .bind(&key.external_session_id)
                            .bind(&key.terminal_id)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_error)?;
                        }
                        LogStream::Raw => {
                            let floor = row.last_offset;
                            sqlx::query(
                                "UPDATE terminal
                                    SET retained_first_offset = MAX(retained_first_offset, ?1)
                                  WHERE session_source = ?2 AND external_session_id = ?3
                                    AND terminal_id = ?4",
                            )
                            .bind(i64_of(floor)?)
                            .bind(&key.session_source)
                            .bind(&key.external_session_id)
                            .bind(&key.terminal_id)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_error)?;
                        }
                    }
                    reclaimed_file_name = Some(row.file_name);
                }
                None => {
                    // Unsafe reclaim: every live row is referenced or
                    // unsealed, so nothing may be reclaimed. The creation
                    // itself never happens and the transaction changes
                    // nothing; the caller records the dropped batch (gap +
                    // watermark + latch) via `drop_batch`.
                    tx.rollback().await.map_err(db_error)?;
                    return Ok(SegmentCreation::UnsafeReclaim);
                }
            }
        }
        // Temporary unique name: replaced by the id-derived name inside the
        // same transaction, so a crash can never leave a row whose file name
        // does not match its id.
        let placeholder = format!(".pending-{}", uuid::Uuid::now_v7().simple());
        let insert = match stream {
            LogStream::Normalized => {
                let first_line = first_line.unwrap_or(1);
                sqlx::query(
                    "INSERT INTO segment
                         (session_source, external_session_id, terminal_id, kind, file_name,
                          first_line, last_line, first_offset, last_offset, created_ms)
                     VALUES (?1, ?2, ?3, 'normalized', ?4, ?5, ?5, 0, 0, ?6)
                     RETURNING segment_id",
                )
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .bind(&placeholder)
                .bind(i64_of(first_line)?)
                .bind(i64_of(paths::now_ms())?)
            }
            LogStream::Raw => sqlx::query(
                "INSERT INTO segment
                     (session_source, external_session_id, terminal_id, kind, file_name,
                      first_line, last_line, first_offset, last_offset, created_ms)
                 VALUES (?1, ?2, ?3, 'raw', ?4, NULL, NULL, ?5, ?5, ?6)
                 RETURNING segment_id",
            )
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .bind(&placeholder)
            .bind(i64_of(first_offset)?)
            .bind(i64_of(paths::now_ms())?),
        };
        let segment_id: i64 = insert.fetch_one(&mut *tx).await.map_err(db_error)?.get(0);
        let file_name = paths::segment_file_name(segment_id);
        sqlx::query("UPDATE segment SET file_name = ?2 WHERE segment_id = ?1")
            .bind(segment_id)
            .bind(&file_name)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(SegmentCreation::Created(InsertedSegment {
            segment_id,
            file_name,
            reclaimed_file_name,
        }))
    }

    async fn terminal_tx(
        tx: &mut sqlx::SqliteConnection,
        key: &LogKey,
    ) -> Result<TerminalRow, StorageError> {
        let row = sqlx::query(
            "SELECT * FROM terminal
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            StorageError::Database(format!(
                "terminal row vanished for {} inside its own transaction",
                key.terminal_id
            ))
        })?;
        row_terminal(&row)
    }

    pub(crate) async fn segment(&self, segment_id: i64) -> Result<SegmentRow, StorageError> {
        let row = sqlx::query("SELECT * FROM segment WHERE segment_id = ?1")
            .bind(segment_id)
            .fetch_one(&self.pool)
            .await
            .map_err(db_error)?;
        row_segment(&row)
    }

    /// All segment rows of one stream, oldest first. Never mixes kinds.
    pub(crate) async fn segments(
        &self,
        key: &LogKey,
        stream: LogStream,
    ) -> Result<Vec<SegmentRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM segment
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND kind = ?4
              ORDER BY segment_id",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(stream.db_text())
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        rows.iter().map(row_segment).collect()
    }

    /// All segment rows of one terminal across both streams, oldest first.
    pub(crate) async fn all_segments(&self, key: &LogKey) -> Result<Vec<SegmentRow>, StorageError> {
        let rows = sqlx::query(
            "SELECT * FROM segment
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
              ORDER BY segment_id",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        rows.iter().map(row_segment).collect()
    }

    pub(crate) async fn seal_segment(&self, segment_id: i64) -> Result<(), StorageError> {
        sqlx::query("UPDATE segment SET state = 'sealed', sealed_ms = ?2 WHERE segment_id = ?1")
            .bind(segment_id)
            .bind(i64_of(paths::now_ms())?)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Record one *uncovered hole* of a stream's ordered chain — a range
    /// the watermark consumed but that no segment backs and no explicit
    /// gap covers — in one transaction: the coalesced stream-scoped gap
    /// for the exact hole plus the permanent `degraded` latch. An
    /// unrecorded loss must never survive a completed recovery pass.
    pub(crate) async fn record_hole(
        &self,
        key: &LogKey,
        stream: LogStream,
        span: GapSpan,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        Self::merge_gap_tx(&mut tx, key, stream, span).await?;
        sqlx::query(
            "UPDATE terminal SET degraded = 1
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    /// Record one irrecoverable indexed segment in **one** transaction:
    /// the coalesced stream-scoped gap for its whole range, the
    /// active-pointer clear (only if it referenced this row), the row's
    /// tombstone, and the permanent `degraded` latch. One commit means a
    /// crash can never leave a half-recorded loss (a gap without its
    /// tombstone, or a loss without the latch); the file quarantine that
    /// follows the transaction is deliberately outside it, and a rerun's
    /// orphan pass converges on the then-unclaimed file.
    pub(crate) async fn record_loss(
        &self,
        key: &LogKey,
        stream: LogStream,
        span: GapSpan,
        segment_id: i64,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        Self::merge_gap_tx(&mut tx, key, stream, span).await?;
        Self::clear_pointer_tx(&mut tx, key, stream, segment_id).await?;
        sqlx::query(
            "DELETE FROM segment
              WHERE segment_id = ?1
                AND session_source = ?2 AND external_session_id = ?3 AND terminal_id = ?4
                AND kind = ?5",
        )
        .bind(segment_id)
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(stream.db_text())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "UPDATE terminal SET degraded = 1
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    /// Coalesce `span` into one stream's persisted gap set inside an open
    /// transaction (load, merge, rewrite the whole set).
    async fn merge_gap_tx(
        tx: &mut sqlx::SqliteConnection,
        key: &LogKey,
        stream: LogStream,
        span: GapSpan,
    ) -> Result<(), StorageError> {
        let mut spans = Self::gaps_tx(tx, key, stream).await?;
        spans.push(span);
        let merged = coalesce(spans, crate::MAX_GAP_RECORDS);
        Self::rewrite_gaps_tx(tx, key, stream, &merged).await?;
        Ok(())
    }

    /// Record one *dropped* batch of one stream — a batch that could not be
    /// persisted (an unsafe retention reclaim) — in one transaction: the
    /// stream-scoped gap for its exact range, the non-reusable watermark
    /// advanced to `watermark` (SQL `MAX`, so it can never decrease), and
    /// the `degraded` + `refuse_new_start` latches. The drain producer
    /// keeps running: only future terminal starts are refused, and every
    /// further append records its own drop through this method, so the
    /// coalesced gap stays one widening record instead of growing the row
    /// count. The reason is `missing`: no segment ever backed the range.
    pub(crate) async fn drop_batch(
        &self,
        key: &LogKey,
        stream: LogStream,
        span: GapSpan,
        watermark: u64,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let existing = Self::gaps_tx(&mut tx, key, stream).await?;
        let mut spans = existing;
        spans.push(span);
        let merged = coalesce(spans, crate::MAX_GAP_RECORDS);
        Self::rewrite_gaps_tx(&mut tx, key, stream, &merged).await?;
        let statement = match stream {
            LogStream::Normalized => {
                "UPDATE terminal
                    SET line_watermark = MAX(line_watermark, ?1),
                        degraded = 1, refuse_new_start = 1
                  WHERE session_source = ?2 AND external_session_id = ?3
                    AND terminal_id = ?4"
            }
            LogStream::Raw => {
                "UPDATE terminal
                    SET raw_watermark = MAX(raw_watermark, ?1),
                        degraded = 1, refuse_new_start = 1
                  WHERE session_source = ?2 AND external_session_id = ?3
                    AND terminal_id = ?4"
            }
        };
        let result = sqlx::query(statement)
            .bind(i64_of(watermark)?)
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        expect_one(&result, "terminal (drop latch)")?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    /// Rewrite one stream's whole gap set inside an open transaction.
    async fn rewrite_gaps_tx(
        tx: &mut sqlx::SqliteConnection,
        key: &LogKey,
        stream: LogStream,
        merged: &[GapSpan],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM log_gap
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND kind = ?4",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(stream.db_text())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        for gap in merged {
            sqlx::query(
                "INSERT INTO log_gap
                     (session_source, external_session_id, terminal_id, kind,
                      range_start, range_end, reason, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .bind(stream.db_text())
            .bind(i64_of(gap.start)?)
            .bind(i64_of(gap.end)?)
            .bind(gap.reason.db_text())
            .bind(i64_of(paths::now_ms())?)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        Ok(())
    }

    /// The coalesced gap set of one stream, oldest first. Read by the
    /// recovery report, the ordered-chain validation, and the test-hooks
    /// recovery snapshot; the S4 query surface consumes it too.
    pub(crate) async fn gaps(
        &self,
        key: &LogKey,
        stream: LogStream,
    ) -> Result<Vec<GapSpan>, StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let gaps = Self::gaps_tx(&mut tx, key, stream).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(gaps)
    }

    async fn gaps_tx(
        tx: &mut sqlx::SqliteConnection,
        key: &LogKey,
        stream: LogStream,
    ) -> Result<Vec<GapSpan>, StorageError> {
        let rows = sqlx::query(
            "SELECT range_start, range_end, reason FROM log_gap
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3
                AND kind = ?4
              ORDER BY range_start",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(stream.db_text())
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        rows.iter()
            .map(|row| {
                let reason: String = row.try_get("reason").map_err(db_error)?;
                Ok(GapSpan {
                    start: u64_of(row.try_get::<i64, _>("range_start").map_err(db_error)?)?,
                    end: u64_of(row.try_get::<i64, _>("range_end").map_err(db_error)?)?,
                    reason: GapReason::from_db_text(&reason).ok_or_else(|| {
                        StorageError::Database(format!("unknown gap reason {reason:?}"))
                    })?,
                })
            })
            .collect()
    }

    /// Tombstone one segment row and clear whichever active pointer still
    /// references it, in one transaction. The pointer clears *before* the
    /// delete (the terminal row's foreign keys reference segment ids), and
    /// the whole thing is idempotent: a rerun matches no row and clears
    /// nothing.
    pub(crate) async fn tombstone_segment(
        &self,
        key: &LogKey,
        segment_id: i64,
        kind: LogStream,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        Self::clear_pointer_tx(&mut tx, key, kind, segment_id).await?;
        // The pointer clears before the delete: the terminal row's foreign
        // keys reference segment ids, so deleting a still-referenced row
        // would fail loudly instead of clearing the pointer.
        sqlx::query(
            "DELETE FROM segment
              WHERE segment_id = ?1
                AND session_source = ?2 AND external_session_id = ?3 AND terminal_id = ?4
                AND kind = ?5",
        )
        .bind(segment_id)
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .bind(kind.db_text())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    /// Clear one stream's active pointer inside an open transaction, only
    /// where it references `segment_id`. Idempotent: a rerun matches no
    /// row and clears nothing.
    async fn clear_pointer_tx(
        tx: &mut sqlx::SqliteConnection,
        key: &LogKey,
        stream: LogStream,
        segment_id: i64,
    ) -> Result<(), StorageError> {
        let statement = match stream {
            LogStream::Normalized => {
                "UPDATE terminal
                    SET active_normalized_segment = NULL
                  WHERE session_source = ?1 AND external_session_id = ?2
                    AND terminal_id = ?3 AND active_normalized_segment = ?4"
            }
            LogStream::Raw => {
                "UPDATE terminal
                    SET active_raw_segment = NULL
                  WHERE session_source = ?1 AND external_session_id = ?2
                    AND terminal_id = ?3 AND active_raw_segment = ?4"
            }
        };
        sqlx::query(statement)
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .bind(segment_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Clear one stream's active pointer when it references a row that is
    /// gone, of the other stream, or no longer active. The referenced row
    /// itself is never touched: a stale pointer is bookkeeping, not a loss.
    pub(crate) async fn clear_active_pointer(
        &self,
        key: &LogKey,
        stream: LogStream,
        segment_id: i64,
    ) -> Result<(), StorageError> {
        let statement = match stream {
            LogStream::Normalized => {
                "UPDATE terminal SET active_normalized_segment = NULL
                  WHERE session_source = ?1 AND external_session_id = ?2
                    AND terminal_id = ?3 AND active_normalized_segment = ?4"
            }
            LogStream::Raw => {
                "UPDATE terminal SET active_raw_segment = NULL
                  WHERE session_source = ?1 AND external_session_id = ?2
                    AND terminal_id = ?3 AND active_raw_segment = ?4"
            }
        };
        sqlx::query(statement)
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .bind(segment_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Clamp one stream's retained floor down to `to` (never up): the
    /// floor may only advance under positions the watermark consumed, so
    /// a floor beyond the watermark-derived expected end is broken
    /// bookkeeping that recovery clamps back under the end. Idempotent —
    /// a rerun matches an already-clamped floor and changes nothing —
    /// and bookkeeping only: no gap is recorded, no row is touched, and
    /// no latch is set.
    pub(crate) async fn clamp_retained_floor(
        &self,
        key: &LogKey,
        stream: LogStream,
        to: u64,
    ) -> Result<(), StorageError> {
        let statement = match stream {
            LogStream::Normalized => {
                "UPDATE terminal
                    SET retained_first_line = MIN(retained_first_line, ?1)
                  WHERE session_source = ?2 AND external_session_id = ?3
                    AND terminal_id = ?4"
            }
            LogStream::Raw => {
                "UPDATE terminal
                    SET retained_first_offset = MIN(retained_first_offset, ?1)
                  WHERE session_source = ?2 AND external_session_id = ?3
                    AND terminal_id = ?4"
            }
        };
        sqlx::query(statement)
            .bind(i64_of(to)?)
            .bind(&key.session_source)
            .bind(&key.external_session_id)
            .bind(&key.terminal_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Latch `degraded` on the terminal (explicit losses exist; the latch
    /// is permanent until a destructive rebuild). Standalone latch for
    /// callers with no gap to record; the loss and hole repairs latch
    /// inside their own transactions.
    #[allow(dead_code)]
    pub(crate) async fn latch_degraded(&self, key: &LogKey) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE terminal SET degraded = 1
              WHERE session_source = ?1 AND external_session_id = ?2 AND terminal_id = ?3",
        )
        .bind(&key.session_source)
        .bind(&key.external_session_id)
        .bind(&key.terminal_id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// The one short visible-index transaction. Must run only after the
    /// file bytes it describes have been fsynced; contains no file I/O
    /// itself. Both watermarks use SQL `MAX` so they can never decrease,
    /// and every statement is guarded by the stream `kind` plus a
    /// rows-affected check, so a commit can never publish one stream's
    /// bytes under the other stream's pointer. The transaction opens with
    /// a compare-and-set on the durable values this writer last published
    /// (the terminal watermark and the receiving segment's indexed range):
    /// a mismatch fails loudly as [`StorageError::CommitConflict`]
    /// instead of letting a second writer's ranges overlap behind the
    /// monotonic `MAX` updates. Single-writer enforcement makes the CAS a
    /// guard, not the serializer — the exclusive lease is the serializer.
    pub(crate) async fn commit_visible(
        &self,
        key: &LogKey,
        input: &CommitInput,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        self.hit(CrashPoint::TxnBegin);
        match input.commit {
            StreamCommit::Normalized {
                segment_last_line,
                line_watermark,
                prior_segment_last_line,
                prior_line_watermark,
            } => {
                let found: Option<(Option<i64>,)> = sqlx::query_as(
                    "SELECT last_line FROM segment
                      WHERE segment_id = ?1
                        AND session_source = ?2 AND external_session_id = ?3
                        AND terminal_id = ?4 AND kind = 'normalized'",
                )
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
                match found {
                    Some((Some(found),)) if u64_of(found)? == prior_segment_last_line => {}
                    Some((found,)) => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "normalized segment {} indexed last_line is {:?} but this \
                                 writer continues from {prior_segment_last_line}",
                                input.segment_id,
                                found.map(u64_of).transpose().ok().flatten()
                            ),
                        });
                    }
                    None => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "normalized segment {} is gone; its receiving row vanished \
                                 since this writer cached it",
                                input.segment_id
                            ),
                        });
                    }
                }
                let found: Option<(i64,)> = sqlx::query_as(
                    "SELECT line_watermark FROM terminal
                      WHERE session_source = ?1 AND external_session_id = ?2
                        AND terminal_id = ?3",
                )
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
                match found {
                    Some((found,)) if u64_of(found)? == prior_line_watermark => {}
                    Some((found,)) => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "line watermark is {} but this writer continues a batch \
                                 from {prior_line_watermark}",
                                u64_of(found).unwrap_or_default()
                            ),
                        });
                    }
                    None => {
                        return Err(StorageError::Database(
                            "terminal row vanished inside its own commit".into(),
                        ));
                    }
                }
                let result = sqlx::query(
                    "UPDATE segment
                        SET committed_bytes = ?1, fsynced_bytes = ?2,
                            last_line = MAX(last_line, ?3)
                      WHERE segment_id = ?4
                        AND session_source = ?5 AND external_session_id = ?6
                        AND terminal_id = ?7 AND kind = 'normalized'",
                )
                .bind(i64_of(input.committed_bytes)?)
                .bind(i64_of(input.fsynced_bytes)?)
                .bind(i64_of(segment_last_line)?)
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                expect_one(&result, "normalized segment")?;
                let result = sqlx::query(
                    "UPDATE terminal
                        SET line_watermark = MAX(line_watermark, ?1),
                            active_normalized_segment = ?2
                      WHERE session_source = ?3 AND external_session_id = ?4
                        AND terminal_id = ?5",
                )
                .bind(i64_of(line_watermark)?)
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                expect_one(&result, "terminal (normalized)")?;
            }
            StreamCommit::Raw {
                segment_last_offset,
                raw_watermark,
                prior_segment_last_offset,
                prior_raw_watermark,
            } => {
                let found: Option<(i64,)> = sqlx::query_as(
                    "SELECT last_offset FROM segment
                      WHERE segment_id = ?1
                        AND session_source = ?2 AND external_session_id = ?3
                        AND terminal_id = ?4 AND kind = 'raw'",
                )
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
                match found {
                    Some((found,)) if u64_of(found)? == prior_segment_last_offset => {}
                    Some((found,)) => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "raw segment {} indexed last_offset is {} but this writer \
                                 continues from {prior_segment_last_offset}",
                                input.segment_id,
                                u64_of(found).unwrap_or_default()
                            ),
                        });
                    }
                    None => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "raw segment {} is gone; its receiving row vanished \
                                 since this writer cached it",
                                input.segment_id
                            ),
                        });
                    }
                }
                let found: Option<(i64,)> = sqlx::query_as(
                    "SELECT raw_watermark FROM terminal
                      WHERE session_source = ?1 AND external_session_id = ?2
                        AND terminal_id = ?3",
                )
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
                match found {
                    Some((found,)) if u64_of(found)? == prior_raw_watermark => {}
                    Some((found,)) => {
                        return Err(StorageError::CommitConflict {
                            detail: format!(
                                "raw watermark is {} but this writer continues a batch \
                                 from {prior_raw_watermark}",
                                u64_of(found).unwrap_or_default()
                            ),
                        });
                    }
                    None => {
                        return Err(StorageError::Database(
                            "terminal row vanished inside its own commit".into(),
                        ));
                    }
                }
                let result = sqlx::query(
                    "UPDATE segment
                        SET committed_bytes = ?1, fsynced_bytes = ?2,
                            last_offset = MAX(last_offset, ?3)
                      WHERE segment_id = ?4
                        AND session_source = ?5 AND external_session_id = ?6
                        AND terminal_id = ?7 AND kind = 'raw'",
                )
                .bind(i64_of(input.committed_bytes)?)
                .bind(i64_of(input.fsynced_bytes)?)
                .bind(i64_of(segment_last_offset)?)
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                expect_one(&result, "raw segment")?;
                let result = sqlx::query(
                    "UPDATE terminal
                        SET raw_watermark = MAX(raw_watermark, ?1),
                            active_raw_segment = ?2
                      WHERE session_source = ?3 AND external_session_id = ?4
                        AND terminal_id = ?5",
                )
                .bind(i64_of(raw_watermark)?)
                .bind(input.segment_id)
                .bind(&key.session_source)
                .bind(&key.external_session_id)
                .bind(&key.terminal_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
                expect_one(&result, "terminal (raw)")?;
            }
        }
        self.hit(CrashPoint::TxnUpdate);
        self.hit(CrashPoint::TxnBeforeCommit);
        tx.commit().await.map_err(db_error)?;
        self.hit(CrashPoint::TxnAfterCommit);
        Ok(())
    }
}

/// Every visibility statement must match exactly the one row it targets;
/// a miss means the commit carried the wrong stream, id, or key and must
/// fail loudly instead of silently publishing nothing.
fn expect_one(result: &SqliteQueryResult, what: &str) -> Result<(), StorageError> {
    if result.rows_affected() != 1 {
        return Err(StorageError::Database(format!(
            "visibility update matched no {what} row"
        )));
    }
    Ok(())
}
