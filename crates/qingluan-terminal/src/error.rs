//! Public error surface of the terminal runtime.
//!
//! Every variant speaks in core domain types ([`TerminalRef`],
//! [`PartialWrite`]) or plain text; no storage, sqlx, libc, PTY, or wire
//! type appears in a public signature. The crate-private mappers at the
//! bottom translate storage and query failures into these variants, which is
//! the only place a storage error is touched. A partial write carries the exact known byte count
//! and a wire-agnostic [`WriteAbort`](qingluan_core::terminal::WriteAbort)
//! reason; a refused send carries a typed [`SendRejection`].

use std::fmt;

use qingluan_core::terminal::{
    HistoryPosition, HistoryRange, PartialWrite, QueryError, TerminalRef,
};

/// Why a send was refused before any byte was handed to the PTY.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRejection {
    /// The payload is larger than the accepted single-send bound.
    Oversize,
    /// The bounded write queue is full (two queued plus one in flight).
    QueueFull,
    /// A stop flow is committed; the terminal no longer accepts input.
    Stopped,
    /// The supplied control generation is no longer current.
    ControlLost,
    /// The terminal is not known to this runtime.
    Unknown,
}

impl fmt::Display for SendRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendRejection::Oversize => write!(f, "payload exceeds the single-send bound"),
            SendRejection::QueueFull => write!(f, "write queue is full"),
            SendRejection::Stopped => write!(f, "terminal is stopping"),
            SendRejection::ControlLost => write!(f, "control generation is no longer current"),
            SendRejection::Unknown => write!(f, "terminal is not known"),
        }
    }
}

/// Outcome of a refused or partially written send.
///
/// Success is a [`SendReceipt`](qingluan_core::terminal::SendReceipt); this
/// type is the failure side only. A [`SendError::Partial`] is a known,
/// exact count (including zero) — an unknown count is never represented.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// No byte was written: the request was refused at its commit point.
    Rejected(SendRejection),
    /// The write started and stopped early; `written_bytes` is exact.
    Partial(PartialWrite),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Rejected(reason) => write!(f, "send rejected: {reason}"),
            SendError::Partial(partial) => write!(
                f,
                "partial write: {} bytes written before {:?}",
                partial.written_bytes, partial.reason
            ),
        }
    }
}

impl std::error::Error for SendError {}

/// Which activity quota rejected a terminal start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaScope {
    /// The owning session reached its terminal limit.
    Session,
    /// The daemon-wide terminal limit was reached.
    Global,
}

/// Failure of a terminal lifecycle operation.
#[non_exhaustive]
#[derive(Debug)]
pub enum RuntimeError {
    /// No terminal with this identity is known to the runtime.
    UnknownTerminal(TerminalRef),
    /// A supplied control generation is no longer current.
    ControlLost(TerminalRef),
    /// The control-generation sequence is exhausted (never wraps onto a
    /// live generation).
    ControlGenerationExhausted,
    /// The terminal no longer accepts input or resize.
    NotWritable(TerminalRef),
    /// An activity quota rejected the start before any slot was reserved.
    QuotaExhausted {
        /// Which independently enforced quota was full.
        scope: QuotaScope,
    },
    /// The start transaction failed before the terminal was running.
    StartRejected {
        /// Identity minted for the failed start.
        terminal: TerminalRef,
        /// Human-readable detail (already classified).
        detail: String,
    },
    /// Cleanup could not be verified; the quota slot stays occupied and is
    /// deliberately never released.
    CleanupIncomplete {
        /// Terminal whose cleanup is incomplete.
        terminal: TerminalRef,
        /// What could not be verified.
        detail: String,
    },
    /// The persistence seam failed.
    Storage {
        /// Human-readable detail.
        detail: String,
    },
    /// A cgroup primitive failed (delegation or `cgroup.kill` missing, or a
    /// write/removal error).
    Cgroup {
        /// Human-readable detail.
        detail: String,
    },
    /// The runtime is shutting down and no longer accepts starts.
    Shutdown,
    /// A query position is no longer readable: the cursor's log identity or
    /// epoch no longer matches, the retained history no longer reaches it,
    /// it falls inside an explicitly recorded gap, or it is an overwritten
    /// (or still mutable) tail revision. `earliest` is the earliest
    /// readable position when it is known and `missing` the exact missing
    /// range when it is known. Recovery is the caller's explicit choice:
    /// the position is never silently re-anchored.
    CursorExpired {
        /// Earliest position still readable, if known.
        earliest: Option<HistoryPosition>,
        /// Exact missing range, if known.
        missing: Option<HistoryRange>,
    },
    /// The requested fixed range intersects an explicitly recorded gap; the
    /// caller must choose the range on the other side, because a scan never
    /// claims to have covered a hole.
    QueryGap {
        /// The missing range.
        range: HistoryRange,
    },
    /// The query is inconsistent (a cursor minted for another log, a scan
    /// point reused after the needle or options changed, an empty needle).
    InvalidQuery {
        /// Human-readable detail.
        detail: String,
    },
    /// A non-write lifecycle operation failed at the OS level.
    Io {
        /// Human-readable detail.
        detail: String,
    },
    /// An event replay or subscription named a sequence strictly before the
    /// session's pruned bound, so the requested range is no longer retained.
    /// The cleared prefix and the earliest recoverable resumption bound are
    /// carried whole; recovery is the caller's explicit choice and never a
    /// silent jump to the newest event.
    EventRangeCleared {
        /// The requested exclusive lower bound.
        after_event_seq: u64,
        /// Highest already-pruned sequence for this session.
        pruned_through_seq: u64,
        /// Earliest safe resumption bound (== `pruned_through_seq`).
        available_after_seq: u64,
    },
    /// A cumulative event ack named a sequence beyond the session's durable
    /// committed bound: an uncommitted bound may never be acknowledged.
    /// Nothing was written.
    EventAckOutOfBounds {
        /// The refused acknowledgement bound.
        up_to_seq: u64,
        /// The committed bound it exceeded.
        last_committed_seq: u64,
    },
    /// One or more terminals could not be verifiably cleaned up during
    /// shutdown.
    ShutdownIncomplete {
        /// What could not be verified.
        detail: String,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::UnknownTerminal(terminal) => {
                write!(f, "unknown terminal {}", terminal.terminal_id.as_str())
            }
            RuntimeError::ControlLost(terminal) => write!(
                f,
                "control generation lost for terminal {}",
                terminal.terminal_id.as_str()
            ),
            RuntimeError::NotWritable(terminal) => write!(
                f,
                "terminal {} no longer accepts input",
                terminal.terminal_id.as_str()
            ),
            RuntimeError::ControlGenerationExhausted => {
                write!(f, "control generation sequence exhausted")
            }
            RuntimeError::QuotaExhausted { scope } => {
                write!(f, "{scope:?} terminal quota exhausted")
            }
            RuntimeError::StartRejected { terminal, detail } => write!(
                f,
                "start of terminal {} rejected: {detail}",
                terminal.terminal_id.as_str()
            ),
            RuntimeError::CleanupIncomplete { terminal, detail } => write!(
                f,
                "cleanup of terminal {} incomplete: {detail}",
                terminal.terminal_id.as_str()
            ),
            RuntimeError::Storage { detail } => write!(f, "storage error: {detail}"),
            RuntimeError::Cgroup { detail } => write!(f, "cgroup error: {detail}"),
            RuntimeError::Shutdown => write!(f, "runtime is shutting down"),
            RuntimeError::CursorExpired { earliest, missing } => write!(
                f,
                "query position is no longer readable: earliest {earliest:?}, missing {missing:?}"
            ),
            RuntimeError::QueryGap { range } => {
                write!(f, "history is missing in {range:?}")
            }
            RuntimeError::InvalidQuery { detail } => write!(f, "invalid query: {detail}"),
            RuntimeError::Io { detail } => write!(f, "io error: {detail}"),
            RuntimeError::EventRangeCleared {
                after_event_seq,
                pruned_through_seq,
                available_after_seq,
            } => write!(
                f,
                "event range cleared: after_event_seq {after_event_seq} precedes pruned bound \
                 {pruned_through_seq}; resume after {available_after_seq}"
            ),
            RuntimeError::EventAckOutOfBounds {
                up_to_seq,
                last_committed_seq,
            } => write!(
                f,
                "event ack {up_to_seq} exceeds committed event bound {last_committed_seq}"
            ),
            RuntimeError::ShutdownIncomplete { detail } => {
                write!(f, "shutdown incomplete: {detail}")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Map a terminal-scoped storage failure, preserving an unknown terminal as
/// `UnknownTerminal` rather than misclassifying it as infrastructure loss.
pub(crate) fn storage_error_for_terminal(
    error: qingluan_storage::StorageError,
    terminal: &TerminalRef,
) -> RuntimeError {
    match error {
        qingluan_storage::StorageError::UnknownLog(_) => {
            RuntimeError::UnknownTerminal(terminal.clone())
        }
        other => storage_error(other),
    }
}

/// Map a storage failure onto the runtime's public error surface. A query
/// refusal keeps its domain reason (expiry, gap, or a malformed request);
/// everything else is a storage fault.
pub(crate) fn storage_error(error: qingluan_storage::StorageError) -> RuntimeError {
    match error {
        qingluan_storage::StorageError::Query(QueryError::CursorExpired { earliest, missing }) => {
            RuntimeError::CursorExpired { earliest, missing }
        }
        qingluan_storage::StorageError::Query(QueryError::Gap { range }) => {
            RuntimeError::QueryGap { range }
        }
        qingluan_storage::StorageError::Query(QueryError::Invalid { detail }) => {
            RuntimeError::InvalidQuery { detail }
        }
        qingluan_storage::StorageError::EventRangeCleared {
            after,
            pruned_through_seq,
            available_after_seq,
        } => RuntimeError::EventRangeCleared {
            after_event_seq: after,
            pruned_through_seq,
            available_after_seq,
        },
        qingluan_storage::StorageError::EventAckOutOfBounds { acked, committed } => {
            RuntimeError::EventAckOutOfBounds {
                up_to_seq: acked,
                last_committed_seq: committed,
            }
        }
        other => RuntimeError::Storage {
            detail: other.to_string(),
        },
    }
}

/// Map a query-value failure onto the runtime's public error surface.
pub(crate) fn query_error(error: QueryError) -> RuntimeError {
    match error {
        QueryError::CursorExpired { earliest, missing } => {
            RuntimeError::CursorExpired { earliest, missing }
        }
        QueryError::Gap { range } => RuntimeError::QueryGap { range },
        QueryError::Invalid { detail } => RuntimeError::InvalidQuery { detail },
        other => RuntimeError::InvalidQuery {
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingluan_core::terminal::{ExternalSessionId, SessionRef, SessionSource, TerminalId};

    #[test]
    fn unknown_log_remains_a_terminal_not_found_error() {
        let terminal = TerminalRef {
            session: SessionRef {
                source: SessionSource::new("test"),
                external_id: ExternalSessionId::new("session"),
            },
            terminal_id: TerminalId::new("terminal"),
        };
        assert!(matches!(
            storage_error_for_terminal(
                qingluan_storage::StorageError::UnknownLog("terminal".into()),
                &terminal,
            ),
            RuntimeError::UnknownTerminal(ref unknown) if unknown == &terminal
        ));
    }
}
