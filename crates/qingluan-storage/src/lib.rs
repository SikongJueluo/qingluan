//! Local persistence for terminal output logs (S2 foundation).
//!
//! One storage root holds a SQLite database (`terminal.db`) plus immutable
//! kind-tagged segment files (`seg-NNNNNN.log`) written by [`LogWriter`].
//! Two independent streams share one durable format: the raw stream keeps
//! arbitrary output bytes for archival (including NUL and invalid UTF-8)
//! and the normalized stream keeps UTF-8 line text with 1-based
//! `(line, byte_offset)` positions. Streams never share segments, frame
//! sequences, active pointers, or watermarks, so raw and normalized
//! positions can never be cross-used. The format is fixed: a 64-byte
//! segment header, 40-byte frame headers, payloads of at most 64 KiB, and
//! IEEE CRC32 over the header and payload regions; normalized payloads
//! cut only on UTF-8 character boundaries, raw payloads on arbitrary byte
//! boundaries. Continuity (`frame_seq`, line numbering, offsets) is
//! validated with checked arithmetic on every scan.
//!
//! Durability contract (verified at the public seam): appended frames are
//! fsynced before the short visibility transaction runs, that transaction
//! contains no file I/O and never waits on a client, and in-memory state
//! is published only after it commits. Bytes fsynced but not yet committed
//! are invisible to every read. The parent directory is fsynced before a
//! new segment's first frame. All file I/O runs on the async runtime's
//! blocking pool; no async method blocks the executor.
//!
//! Identity at this seam: `TerminalId` and `LogEpoch` are qingluan-minted
//! canonical UUID text (exactly one spelling per 16-byte identity);
//! SQLite keeps the exact text keys while the segment headers carry the
//! parsed 16-byte UUIDs. `SessionSource` and `ExternalSessionId` stay
//! opaque text. No sqlx, frame, or wire type crosses the public API.
//!
//! Persistence policy: appends join a per-stream pending batch flushed
//! under the confirmed 64 KiB-or-50 ms bound (whichever comes first) —
//! enforced inside this crate by the writer's flush driver, PTY- and
//! caller-independent; an append reaching the byte bound flushes
//! synchronously, a trailing batch is committed within the delay, and
//! [`LogWriter::flush`] forces a boundary. A whole batch is atomic:
//! frames are appended, `sync_data`ed, and committed in one short
//! transaction per stream (lines may split at the 64 KiB payload bound
//! on UTF-8 character boundaries; raw chunks on byte boundaries).
//! Segment rotation happens at 4 MiB, independently per stream, and the
//! per-terminal metadata budget keeps at most 64 live segment rows
//! across both streams combined: the creation that would exceed it
//! transactionally reclaims the oldest sealed segment and advances the
//! retained floor of its stream; when no sealed segment can be
//! reclaimed safely the batch is dropped as an explicit stream-scoped
//! gap with its watermark advanced (numbers never reused), the terminal
//! latches `degraded` + `refuse_new_start`, and the already-running
//! writer keeps draining — only future terminal starts are refused.
//!
//! Ownership: exactly one writer per log, enforced across every store
//! handle and every process sharing the root by an OS file lease that the
//! kernel releases when the holder dies (so a crashed writer never leaves
//! a stale lease) and acquired *before* the terminal row is read, so a
//! waiter queued behind a closing owner attaches at that owner's final
//! durable watermarks and latches. The visibility transaction additionally
//! compare-and-sets the watermark and receiving range it continues from,
//! refusing as a typed conflict instead of letting two writers' ranges
//! overlap behind a monotonic update. Recovery holds the same lease for
//! its pass, and validates each stream's ordered cross-segment chain
//! against its retained floor, watermark, and explicit gaps (as do the
//! verification reads): an overlap, a range extending past the watermark,
//! or an uncovered hole is repaired as an explicit loss, never silently
//! narrowed. [`LogWriter::close`] is the graceful shutdown — it joins the
//! flush driver and returns the final per-stream outcomes, falling back to
//! the most recent batch-bearing outcome a stream already produced when
//! its final drain finds nothing pending — while dropping a writer stays
//! non-durable (buffered bytes are not flushed) and never aborts the flush
//! driver: the lease releases only once the detached driver finishes any
//! in-flight file operation and exits.
//!
//! Recovery ([`LogStore::recover`]) is one idempotent pass per terminal:
//! orphan segment files are quarantined whole and never adopted; a
//! segment row with zero committed bytes is tombstoned whole with its
//! file quarantined (a crashed creation, never adopted — not even a
//! header-valid pending file); an uncommitted tail is persisted as a
//! quarantine artifact before the live file is truncated back to its
//! committed boundary; a missing/truncated/corrupt/row-inconsistent
//! indexed segment becomes an explicit stream-scoped gap plus a
//! permanent `degraded` latch while its watermarks never decrease and
//! its numbers are never reused — recorded, with the active-pointer
//! clear and the row tombstone, in **one** transaction, so a crash can
//! never leave a half-recorded loss. Every repair's file sync and
//! directory sync errors propagate. The public report is domain-level
//! (`repaired`/`degraded`/gap state); segment ids, file names,
//! quarantine artifacts, and truncation boundaries stay under
//! `test-hooks`. A normal restart or rotation preserves the
//! log epoch; only a destructive database rebuild (deleting
//! `terminal.db` and its sidecars) changes it, and the old epoch's
//! identity is then rejected while its files are quarantined.
//!
//! The fixed-range read / tail / grep query API belongs to S4 and is not
//! part of this seam; only the committed-prefix verification scan
//! exists, under `test-hooks`.

mod crash;
mod db;
mod error;
mod frame;
mod gap;
mod identity;
mod lease;
mod paths;
mod recovery;
mod runtime;
mod writer;

#[cfg(any(test, feature = "test-hooks"))]
mod reader;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qingluan_core::terminal::LogIdentity;

pub use error::StorageError;
pub use gap::GapReason;
pub use recovery::{RecoveryGap, RecoveryReport};
pub use runtime::{RuntimePhase, RuntimeRecord, RuntimeRegistry};
pub use writer::{
    AppendOutcome, AppendedLine, AppendedLoss, AppendedRaw, FlushOutcomes, LogWriter,
    StreamFlushOutcome,
};

#[cfg(any(test, feature = "test-hooks"))]
pub use crash::{CrashPoint, CrashSink, ParkPoint};

#[cfg(any(test, feature = "test-hooks"))]
pub use recovery::RecoveryAction;

#[cfg(any(test, feature = "test-hooks"))]
pub use writer::FaultSite;

/// Maximum payload of one frame: the confirmed 64 KiB bound. Larger
/// appends split into multiple frames (normalized: on UTF-8 character
/// boundaries; raw: on arbitrary byte boundaries) and one flushed batch
/// commits atomically. Format/layout constant: crate-private in
/// production builds, exposed only behind test hooks.
#[cfg(any(test, feature = "test-hooks"))]
pub const MAX_FRAME_PAYLOAD: usize = frame::MAX_PAYLOAD as usize;

/// Segment rotation threshold (production value): a stream's segment is
/// sealed before an append that would push it past 4 MiB. Each stream
/// rotates independently. Storage-layout constant: crate-private in
/// production builds, exposed only behind test hooks.
#[cfg(any(test, feature = "test-hooks"))]
pub const MAX_SEGMENT_BYTES: u64 = writer::MAX_SEGMENT_BYTES;

/// Persistence-policy flush delay (50 ms): a pending batch is committed
/// at most this long after its first accepted byte. Enforced by the
/// writer's internal flush driver. Storage-batching detail: crate-private
/// in production builds, exposed only behind test hooks.
#[cfg(any(test, feature = "test-hooks"))]
pub use writer::FLUSH_MAX_DELAY;

/// Persistence-policy flush bound (64 KiB): a pending batch holding this
/// many bytes is flushed by the append that reached it. Storage-batching
/// detail: crate-private in production builds, exposed only behind test
/// hooks.
#[cfg(any(test, feature = "test-hooks"))]
pub use writer::FLUSH_MAX_BYTES;

/// Per-terminal budget of live segment metadata rows, counted across both
/// streams combined (production value). The creation that would exceed it
/// transactionally reclaims the oldest sealed segment and advances the
/// retained floor of its stream; an unsafe reclaim (no sealed segment
/// reclaimable) latches `degraded` + `refuse_new_start` instead.
pub const MAX_SEGMENT_METADATA_ROWS: usize = 64;

/// Upper bound of coalesced gap records per stream (production value).
/// Adjacent or overlapping losses of one stream merge into one record;
/// above the bound, conservative coarsening merges the pair with the
/// smallest hole between them, declaring the swallowed intact range
/// missing rather than growing the row count.
pub const MAX_GAP_RECORDS: usize = 1024;

/// Stream discriminator of the terminal log. The design baseline keeps
/// both: raw output for archival, normalized text for read/tail/grep.
/// Segments, frame sequences, active pointers, and watermarks are
/// per-stream; cursors and reads carry the stream so the two streams can
/// never be cross-addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    /// Normalized UTF-8 line text; positions are 1-based
    /// `(line, byte_offset)` pairs.
    Normalized,
    /// Arbitrary raw output bytes (NUL and invalid UTF-8 included);
    /// positions are stream byte offsets.
    Raw,
}

impl LogStream {
    /// Segment-header `kind` byte of this stream.
    pub(crate) fn segment_kind(self) -> u8 {
        match self {
            LogStream::Normalized => frame::SEGMENT_KIND_NORMALIZED,
            LogStream::Raw => frame::SEGMENT_KIND_RAW,
        }
    }

    /// SQLite `segment.kind` text of this stream.
    pub(crate) fn db_text(self) -> &'static str {
        match self {
            LogStream::Normalized => "normalized",
            LogStream::Raw => "raw",
        }
    }

    /// The stream of a SQLite `segment.kind` text, if it is a known kind.
    pub(crate) fn from_db_text(text: &str) -> Option<Self> {
        match text {
            "normalized" => Some(LogStream::Normalized),
            "raw" => Some(LogStream::Raw),
            _ => None,
        }
    }
}

/// Handle to one storage root. Cheap to clone; all methods are read-side
/// except [`LogStore::open_writer`].
#[derive(Clone)]
pub struct LogStore {
    store: Arc<db::Store>,
    root: PathBuf,
}

impl LogStore {
    /// Open (creating if needed) the storage root: `<root>/terminal.db`
    /// with WAL, `synchronous=FULL`, foreign keys, and a 5 s busy timeout
    /// on a single connection; versioned migrations are applied
    /// transactionally and the persisted format version is verified.
    pub async fn open(root: &Path) -> Result<LogStore, StorageError> {
        let store = db::Store::open(root).await?;
        Ok(LogStore {
            store,
            root: root.to_path_buf(),
        })
    }

    /// Open the writer for one log. Creates the terminal row when absent;
    /// refuses (typed errors) on `refuse_new_start`, an epoch mismatch, or
    /// a persisted terminal identity that does not match the requested
    /// one. A `degraded` latch (explicit gaps exist) does not block
    /// writing: numbering continues at watermark + 1. Requires recovery
    /// (typed error, not repair) when an active segment's on-disk state
    /// does not match its committed boundary. Each stream attaches its own
    /// active segment.
    ///
    /// Exactly one writer per log is enforced across every store handle
    /// and every process sharing this root, for the writer's whole
    /// lifetime: a second attach fails with
    /// [`StorageError::WriterAlreadyActive`] until the first writer is
    /// [`LogWriter::close`]d (or dropped and its flush driver has exited,
    /// or its process died — the OS lease is kernel-released on death).
    pub async fn open_writer(&self, log: &LogIdentity) -> Result<LogWriter, StorageError> {
        LogWriter::attach(Arc::clone(&self.store), &self.root, log).await
    }

    /// A durable runtime registry sharing this store's single SQLite
    /// connection.
    ///
    /// A caller that needs both the log writer and the runtime registry for
    /// one storage root (the terminal runtime) must use this instead of a
    /// second [`RuntimeRegistry::open`]: two connections to one SQLite file
    /// can deadlock on a WAL write-lock upgrade under concurrent writes
    /// (the writer's flush driver races the registry). One root, one
    /// connection, no cross-connection lock.
    pub fn runtime_registry(&self) -> RuntimeRegistry {
        RuntimeRegistry::from_shared(Arc::clone(&self.store))
    }

    /// Run the recovery pass of one terminal log (creating the terminal
    /// row when absent, with the same identity guards as a writer
    /// attach). Idempotent: a completed pass rerun takes zero actions and
    /// observes an equal durable state. The full contract is documented on
    /// this crate's recovery module. The pass holds the log's exclusive
    /// writer lease for its duration and therefore refuses with
    /// [`StorageError::WriterAlreadyActive`] while a writer is attached.
    /// The returned report is domain-level: whether anything was repaired,
    /// the post-pass `degraded` latch, and the explicit gap state — no
    /// segment ids, file names, quarantine artifacts, or truncation
    /// boundaries (those stay under `test-hooks`).
    pub async fn recover(&self, log: &LogIdentity) -> Result<RecoveryReport, StorageError> {
        recovery::recover(&self.store, &self.root, log).await
    }

    /// The comparable durable state of one terminal log (terminal row,
    /// segment rows, gap records, segment and quarantine files with
    /// content checksums). Verification surface for the recovery
    /// idempotence proofs only (test builds; the query read API belongs
    /// to S4).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub async fn recovery_snapshot(
        &self,
        log: &LogIdentity,
    ) -> Result<recovery::RecoverySnapshot, StorageError> {
        recovery::snapshot(&self.store, &self.root, log).await
    }

    /// Install a failpoint observer shared by every writer and recovery
    /// pass of this store (test builds only).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub fn set_crash_sink(&self, sink: Option<crash::CrashSink>) {
        self.store.set_crash_sink(sink);
    }

    /// Open with a runtime-loaded migration directory (test builds only,
    /// for migration-rollback scenarios).
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub async fn open_with_migration_dir(
        root: &Path,
        dir: &Path,
    ) -> Result<LogStore, StorageError> {
        let migrator = sqlx::migrate::Migrator::new(dir)
            .await
            .map_err(error::migrate_error)?;
        let store = db::Store::open_with_migrator(root, migrator).await?;
        Ok(LogStore {
            store,
            root: root.to_path_buf(),
        })
    }
}
