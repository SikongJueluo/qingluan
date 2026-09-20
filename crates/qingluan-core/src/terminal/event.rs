//! Session lifecycle events and their acknowledgment watermarks.

use std::num::NonZeroU64;

use super::ids::TerminalRef;
use super::snapshot::{ExitResult, OutputEnd};

/// Cumulative watermark bounds for one session's persistent event stream.
///
/// Invariant (enforced by [`SessionEventState::new`]):
/// `pruned_through_seq <= acked_through_seq <= last_committed_seq`, with
/// every value starting at zero. The transitions themselves (ack, prune)
/// belong to the persistence slice and are intentionally not modeled here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEventState {
    pruned_through_seq: u64,
    acked_through_seq: u64,
    last_committed_seq: u64,
}

/// A [`SessionEventState`] would violate its watermark ordering.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WatermarkError {
    /// `pruned_through_seq` would exceed `acked_through_seq`.
    #[error("pruned_through_seq {pruned} exceeds acked_through_seq {acked}")]
    PrunedExceedsAcked {
        /// Offending pruned bound.
        pruned: u64,
        /// Ack bound it exceeded.
        acked: u64,
    },
    /// `acked_through_seq` would exceed `last_committed_seq`.
    #[error("acked_through_seq {acked} exceeds last_committed_seq {committed}")]
    AckedExceedsCommitted {
        /// Offending acked bound.
        acked: u64,
        /// Committed bound it exceeded.
        committed: u64,
    },
}

impl SessionEventState {
    /// Bound-checked construction.
    ///
    /// Takes the bounds in invariant order (pruned, acked, committed) and
    /// rejects any inversion of `pruned <= acked <= committed`; the
    /// all-zero initial state is valid. Protocol field order is an adapter
    /// concern and does not shape this domain constructor.
    pub fn new(
        pruned_through_seq: u64,
        acked_through_seq: u64,
        last_committed_seq: u64,
    ) -> Result<Self, WatermarkError> {
        if pruned_through_seq > acked_through_seq {
            return Err(WatermarkError::PrunedExceedsAcked {
                pruned: pruned_through_seq,
                acked: acked_through_seq,
            });
        }
        if acked_through_seq > last_committed_seq {
            return Err(WatermarkError::AckedExceedsCommitted {
                acked: acked_through_seq,
                committed: last_committed_seq,
            });
        }
        Ok(Self {
            pruned_through_seq,
            acked_through_seq,
            last_committed_seq,
        })
    }

    /// Highest event seq already pruned (cleaned) for this session.
    pub fn pruned_through_seq(&self) -> u64 {
        self.pruned_through_seq
    }

    /// Highest event seq cumulatively acknowledged by the controlling
    /// client.
    pub fn acked_through_seq(&self) -> u64 {
        self.acked_through_seq
    }

    /// Highest event seq durably committed (and therefore publishable).
    pub fn last_committed_seq(&self) -> u64 {
        self.last_committed_seq
    }
}

/// Non-zero sequence of one persistent session event.
///
/// Watermark zero means “no event yet”; a real event therefore cannot use
/// zero. Allocation and monotonicity belong to the persistence slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventSequence(NonZeroU64);

impl EventSequence {
    /// Construct a sequence; zero is reserved for empty watermarks.
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    /// Numeric sequence value.
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// One persistent lifecycle event about a session's terminal.
///
/// Carries the full [`TerminalRef`] so a session/terminal mismatch is
/// unrepresentable and replay stays self-explanatory even after the
/// terminal record is deleted; the daemon adapter splits it into the
/// protocol's separate session and terminal-id fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    /// Terminal the event is about.
    pub terminal: TerminalRef,
    /// Persistent, monotonically allocated sequence number within the
    /// session; never reused once public.
    pub event_seq: EventSequence,
    /// What happened.
    pub payload: SessionEventPayload,
}

/// Lifecycle event payloads.
///
/// Only the root process's exit produces a normal completion notification;
/// `OutputClosed` never wakes an extra one. Events carry no output text.
/// The protocol explicitly leaves further lifecycle event kinds pending,
/// so this enum is marked `#[non_exhaustive]`: adding a payload variant
/// must not become a breaking match change for code outside this crate.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEventPayload {
    /// The root process exited with a known result.
    ProcessExited(ExitResult),
    /// Output reading ended.
    OutputClosed(OutputEnd),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{ExternalSessionId, SessionRef, SessionSource, TerminalId};

    #[test]
    fn session_event_state_orders_watermarks() {
        // Valid: the all-zero initial state.
        let zero = SessionEventState::new(0, 0, 0).expect("all-zero initial state is valid");
        assert_eq!(
            (
                zero.acked_through_seq(),
                zero.last_committed_seq(),
                zero.pruned_through_seq()
            ),
            (0, 0, 0)
        );

        // Valid: any pruned <= acked <= committed triple...
        let advanced = SessionEventState::new(2, 5, 9).expect("2 <= 5 <= 9 is valid");
        assert_eq!(
            (
                advanced.acked_through_seq(),
                advanced.last_committed_seq(),
                advanced.pruned_through_seq()
            ),
            (5, 9, 2)
        );

        // ...including fully equal bounds.
        let equal = SessionEventState::new(7, 7, 7).expect("equal bounds are valid");
        assert_eq!(equal.acked_through_seq(), 7);
        assert_eq!(equal.last_committed_seq(), 7);
        assert_eq!(equal.pruned_through_seq(), 7);

        // Invalid: pruned > acked.
        assert_eq!(
            SessionEventState::new(6, 5, 9),
            Err(WatermarkError::PrunedExceedsAcked {
                pruned: 6,
                acked: 5
            })
        );

        // Invalid: acked > committed, even when pruned is fine.
        assert_eq!(
            SessionEventState::new(3, 5, 4),
            Err(WatermarkError::AckedExceedsCommitted {
                acked: 5,
                committed: 4
            })
        );

        // Invalid: pruned > committed is always caught by the chain
        // (here pruned 9 > acked 2).
        assert!(SessionEventState::new(9, 2, 3).is_err());

        // Invalid: ack beyond the committed bound.
        assert!(SessionEventState::new(2, 10, 9).is_err());
    }

    #[test]
    fn session_event_carries_full_terminal_identity() {
        let event = SessionEvent {
            terminal: TerminalRef {
                session: SessionRef {
                    source: SessionSource::new("pi"),
                    external_id: ExternalSessionId::new("s1"),
                },
                terminal_id: TerminalId::new("t1"),
            },
            event_seq: EventSequence::new(11).expect("event sequences start at 1"),
            payload: SessionEventPayload::ProcessExited(ExitResult::ExitCode(0)),
        };
        assert_eq!(event.event_seq.get(), 11);
        assert_eq!(EventSequence::new(0), None);
        assert_eq!(event.terminal.terminal_id.as_str(), "t1");
        assert_eq!(
            event.terminal.session,
            SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("s1"),
            }
        );
        assert_eq!(
            event.payload,
            SessionEventPayload::ProcessExited(ExitResult::ExitCode(0))
        );
        assert_ne!(
            event.payload,
            SessionEventPayload::OutputClosed(OutputEnd::Eof)
        );
    }
}
