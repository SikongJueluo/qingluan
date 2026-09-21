//! Public error surface of the terminal runtime.
//!
//! Every variant speaks in core domain types ([`TerminalRef`],
//! [`PartialWrite`]) or plain text; no storage, sqlx, libc, PTY, or wire
//! type appears here. A partial write carries the exact known byte count
//! and a wire-agnostic [`WriteAbort`](qingluan_core::terminal::WriteAbort)
//! reason; a refused send carries a typed [`SendRejection`].

use std::fmt;

use qingluan_core::terminal::{PartialWrite, TerminalRef};

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
    /// A non-write lifecycle operation failed at the OS level.
    Io {
        /// Human-readable detail.
        detail: String,
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
            RuntimeError::Io { detail } => write!(f, "io error: {detail}"),
            RuntimeError::ShutdownIncomplete { detail } => {
                write!(f, "shutdown incomplete: {detail}")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}
