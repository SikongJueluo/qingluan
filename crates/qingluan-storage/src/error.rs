//! Storage failure taxonomy for the S2 terminal log seam.
//!
//! The public surface must not leak sqlx or frame types: database and
//! migration failures are carried as messages.

use std::path::PathBuf;

use qingluan_core::terminal::QueryError;
use thiserror::Error;

/// Typed failure of the terminal log storage layer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A `TerminalId`/`LogEpoch` value is not qingluan-minted UUID text at
    /// this seam. Raised before any file or database mutation.
    #[error("{field} is not UUID text: {value:?}")]
    InvalidIdentity { field: &'static str, value: String },

    /// Filesystem failure on a storage-owned path.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// SQLite failure (message only; no sqlx type crosses the seam).
    #[error("sqlite error: {0}")]
    Database(String),

    /// Migration failure; no half DDL and no recorded version survive.
    #[error("migration error: {0}")]
    Migration(String),

    /// The persisted format version cannot be read by this build.
    #[error("unsupported persisted format version {found}; this build reads {supported}")]
    FormatVersionUnsupported { found: String, supported: String },

    /// No terminal row exists for the requested log.
    #[error("unknown log {0}")]
    UnknownLog(String),

    /// A runtime-registry operation is not a legal transition from the
    /// record's current durable state (a duplicate begin, an illegal phase
    /// change, or an attempt to overwrite a known outcome).
    #[error("runtime registry conflict: {detail}")]
    RuntimeConflict { detail: String },

    /// A cumulative event ack named a sequence beyond the session's durable
    /// `last_committed_seq`: an uncommitted bound may never be acknowledged.
    /// Nothing is written; the offending bound and the committed bound it
    /// exceeded are carried whole.
    #[error("event ack {acked} exceeds committed event bound {committed}")]
    EventAckOutOfBounds { acked: u64, committed: u64 },

    /// An explicit prune named a sequence beyond the session's durable
    /// `acked_through_seq`: only an acknowledged contiguous prefix may be
    /// cleared. Nothing is deleted; the offending bound and the acked bound
    /// it exceeded are carried whole.
    #[error("event prune through {through} exceeds acked event bound {acked}")]
    EventPruneOutOfBounds { through: u64, acked: u64 },

    /// A replay/subscribe request started strictly before the session's
    /// `pruned_through_seq`, so the requested range is no longer retained.
    /// The cleared prefix and the earliest recoverable resumption point are
    /// carried instead of silently jumping to the newest event.
    #[error(
        "event range cleared: after_event_seq {after} precedes pruned bound \
         {pruned_through_seq}; resume after {available_after_seq}"
    )]
    EventRangeCleared {
        /// The requested exclusive lower bound.
        after: u64,
        /// Highest already-pruned sequence for this session.
        pruned_through_seq: u64,
        /// Earliest safe resumption bound (== `pruned_through_seq`).
        available_after_seq: u64,
    },

    /// `append_line` must continue at exactly `watermark + 1`; line numbers
    /// are never reused or skipped by the writer (gaps are explicit).
    #[error("line {attempted} does not continue at watermark {watermark} + 1")]
    LineNotSequential { attempted: u64, watermark: u64 },

    /// On-disk state needs the recovery pass before writing or reading
    /// (uncommitted tail, short/corrupt file, missing active segment).
    #[error("recovery required: {detail}")]
    RecoveryRequired { detail: String },

    /// The requested log epoch differs from the persisted one; a
    /// destructive rebuild (later stage) is required, never a silent swap.
    #[error("log epoch mismatch: persisted {stored:?}, requested {requested:?}")]
    EpochMismatch { stored: String, requested: String },

    /// The persisted `terminal_uuid` differs from the one parsed from the
    /// requested identity text. Canonical UUID parsing keeps text keys and
    /// header identities 1:1, so this only fires on a tampered or restored
    /// database — and then it must refuse, never adopt.
    #[error("terminal uuid mismatch: persisted {stored:?}, requested {requested:?}")]
    TerminalUuidMismatch { stored: String, requested: String },

    /// The log is degraded; writing continues only per policy.
    #[error("log is degraded: {detail}")]
    Degraded { detail: String },

    /// A new writer start is refused until an operator rebuild clears it.
    #[error("new writer start refused: {detail}")]
    RefuseNewStart { detail: String },

    /// Another writer holds the exclusive lease of this log. Exactly one
    /// writer per log is enforced across every store handle and every
    /// process sharing the storage root (an OS file lock the kernel
    /// releases when the holder dies), because two writers attaching the
    /// same watermark would commit overlapping ranges. Recovery holds the
    /// same lease for its pass, so it refuses while a writer is attached.
    #[error("another writer is attached to this log: {detail}")]
    WriterAlreadyActive { detail: String },

    /// The visibility transaction's compare-and-set refused: the durable
    /// watermark or the receiving segment's indexed range is no longer
    /// what this writer last published (a second writer, or external
    /// database modification). The commit failed loudly instead of
    /// overlapping two writers' ranges.
    #[error("commit conflict: {detail}")]
    CommitConflict { detail: String },

    /// A query was refused by an invariant rather than by a fault: an
    /// expired position ([`QueryError::CursorExpired`]), an explicitly
    /// recorded gap inside the requested range ([`QueryError::Gap`]), or a
    /// malformed request ([`QueryError::Invalid`]). The domain-level
    /// reason is carried whole so a caller can tell expiry from a bad
    /// request, and a refusal is never a silent skip.
    #[error(transparent)]
    Query(#[from] QueryError),
}

pub(crate) fn db_error(error: sqlx::Error) -> StorageError {
    StorageError::Database(error.to_string())
}

pub(crate) fn migrate_error(error: sqlx::migrate::MigrateError) -> StorageError {
    StorageError::Migration(error.to_string())
}

pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> StorageError {
    let path = path.into();
    move |source| StorageError::Io { path, source }
}

pub(crate) fn u64_of(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value)
        .map_err(|_| StorageError::Database(format!("value {value} is not a valid u64")))
}

pub(crate) fn i64_of(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value)
        .map_err(|_| StorageError::Database(format!("value {value} exceeds i64 range")))
}
