//! Failpoint plumbing for the S2 commit sequence.
//!
//! Production builds carry no crash-exit code path: [`CrashPoint`] sites are
//! inert unless a sink is installed (test builds only). The async park hook
//! pauses the writer between `sync_data` and the visibility transaction so
//! tests can prove committed-prefix invisibility without a second process.

#[cfg(any(test, feature = "test-hooks"))]
use std::future::Future;
#[cfg(any(test, feature = "test-hooks"))]
use std::pin::Pin;
use std::sync::Arc;

/// Sinks observe every durability boundary in the commit sequence. Kept
/// unconditional so the sites themselves are frozen and tested in place.
pub type CrashSink = Arc<dyn Fn(CrashPoint) + Send + Sync>;

/// Named failpoint sites along the durability-critical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// Before any segment-creation step ran.
    SegmentBefore,
    /// Segment row inserted, file not yet created.
    SegmentRowInserted,
    /// Header bytes written, not yet synced.
    SegmentHeaderWritten,
    /// Header synced and parent directory fsynced.
    SegmentHeaderSynced,
    /// Before frame bytes are appended.
    FrameBeforeWrite,
    /// Frames appended, not yet synced.
    FrameAfterWrite,
    /// Frames synced; the visibility transaction has not started.
    FrameAfterSync,
    /// Inside the transaction, before the first statement.
    TxnBegin,
    /// Inside the transaction, after the segment/terminal updates.
    TxnUpdate,
    /// Inside the transaction, right before commit.
    TxnBeforeCommit,
    /// Transaction committed.
    TxnAfterCommit,
    /// Session lifecycle: the `session_event` row and the
    /// `session_state.last_committed_seq` bump are inserted, the
    /// containing transaction has not committed.
    EventInsert,
    /// Session lifecycle: inside the transaction, right before commit
    /// (terminal state change and event together).
    EventBeforeCommit,
    /// Session lifecycle: the transaction committed; the record/event have
    /// not been returned to the caller.
    EventCommit,
    /// Session lifecycle: the committed event is published to the caller
    /// (returned from the transition method). Observable only after
    /// [`CrashPoint::EventCommit`].
    EventPublish,
    /// Commit returned; in-memory state not yet published.
    PublishBefore,
    /// In-memory state published.
    PublishAfter,
    /// Recovery: the quarantine artifact of an uncommitted tail is durable
    /// (file synced, directory entry synced); the live file has not been
    /// truncated back to its committed boundary yet.
    RecoveryArtifactSynced,
    /// Recovery: the database rows describing one irrecoverable segment
    /// (gap + tombstone + cleared active pointer) are committed; the
    /// segment file has not been renamed to its quarantine name yet.
    RecoveryRowsCommitted,
}

/// Test-only async pause inside the commit sequence.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) type ParkFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Point where a test can park the writer mid-commit-sequence.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkPoint {
    /// Frames are appended but not yet synced.
    AfterFrameWrite,
    /// Frames are durable but the visibility transaction has not started.
    AfterFileSync,
    /// The visibility transaction committed; the in-memory state is not
    /// yet published.
    AfterCommit,
}
