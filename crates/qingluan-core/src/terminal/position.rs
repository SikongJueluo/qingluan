//! History positions, retained ranges, and log-bound cursors.

use super::ids::{LogIdentity, TailId};

/// A position, retained range, or cursor would violate a documented
/// invariant.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PositionError {
    /// History line numbers start at 1.
    #[error("history line numbers start at 1, got line {line}")]
    ZeroLine {
        /// The offending line number.
        line: u64,
    },
    /// The next-read position lies beyond the cursor's fixed end bound.
    #[error("next-read line {next_line} is beyond fixed end line {end_line}")]
    NextBeyondEnd {
        /// Line of the next unread byte.
        next_line: u64,
        /// Fixed upper line bound.
        end_line: u64,
    },
    /// The retained range's earliest position lies after its latest.
    #[error("earliest position {earliest:?} is after latest position {latest:?}")]
    ReversedRange {
        /// First readable position.
        earliest: HistoryPosition,
        /// Last known readable position.
        latest: HistoryPosition,
    },
}

/// A position inside the normalized history of one terminal.
///
/// Line numbers start at 1 and are never reused, including across
/// rotation. `byte_offset` is only meaningful inside a line: it continues
/// reading an over-long line whose earlier bytes were truncated or pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryPosition {
    /// 1-based stable history line number.
    line: u64,
    /// Byte offset inside that line (0 = start of line).
    byte_offset: u64,
}

impl HistoryPosition {
    /// Checked construction: history line numbers start at 1.
    pub fn new(line: u64, byte_offset: u64) -> Result<Self, PositionError> {
        if line == 0 {
            return Err(PositionError::ZeroLine { line });
        }
        Ok(Self { line, byte_offset })
    }

    /// 1-based stable history line number.
    pub fn line(&self) -> u64 {
        self.line
    }

    /// Byte offset inside the line (0 = start of line).
    pub fn byte_offset(&self) -> u64 {
        self.byte_offset
    }
}

/// Position inside the mutable, not-yet-finalized tail line.
///
/// `revision` invalidates older positions whenever the tail is
/// overwritten; once the tail is fixed as a history line, readers switch
/// to a [`HistoryPosition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailPosition {
    /// Opaque identifier of the tail line.
    tail_id: TailId,
    /// Revision of the tail contents; overwriting bumps it and invalidates
    /// older positions.
    revision: u64,
    /// Byte offset inside the tail line.
    byte_offset: u64,
}

impl TailPosition {
    /// Construction: the tail is not part of the stable history, so a
    /// tail position carries no ordering invariant to check.
    pub fn new(tail_id: TailId, revision: u64, byte_offset: u64) -> Self {
        Self {
            tail_id,
            revision,
            byte_offset,
        }
    }

    /// Opaque identifier of the tail line.
    pub fn tail_id(&self) -> &TailId {
        &self.tail_id
    }

    /// Revision of the tail contents; overwriting bumps it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Byte offset inside the tail line.
    pub fn byte_offset(&self) -> u64 {
        self.byte_offset
    }
}

/// The retained (readable) span of one terminal's history.
///
/// `earliest` carries the in-line offset of the first still-readable byte:
/// when the prefix of an over-long line was pruned, that line is not
/// readable from offset 0. `latest` is the last known readable position
/// and may advance on a live log; it is not a committed watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRange {
    /// First readable position, with its in-line offset.
    earliest: HistoryPosition,
    /// Last known readable position; may advance as output arrives.
    latest: HistoryPosition,
}

impl HistoryRange {
    /// Checked construction: `earliest` must not lie after `latest`
    /// (compared by line, then byte offset).
    pub fn new(earliest: HistoryPosition, latest: HistoryPosition) -> Result<Self, PositionError> {
        if earliest.line() > latest.line()
            || (earliest.line() == latest.line() && earliest.byte_offset() > latest.byte_offset())
        {
            return Err(PositionError::ReversedRange { earliest, latest });
        }
        Ok(Self { earliest, latest })
    }

    /// First readable position, with its in-line offset.
    pub fn earliest(&self) -> HistoryPosition {
        self.earliest
    }

    /// Last known readable position; may advance as output arrives.
    pub fn latest(&self) -> HistoryPosition {
        self.latest
    }
}

/// Continuation cursor for fixed-range history reads.
///
/// `end_line` is fixed when the read starts and never extends with later
/// output, so a page cannot silently grow. The cursor is valid only for
/// the exact [`LogIdentity`] it was minted against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCursor {
    /// Log this cursor was minted from.
    log: LogIdentity,
    /// Position of the next unread byte.
    next: HistoryPosition,
    /// Upper line bound fixed when the read started; later output never
    /// extends it.
    end_line: u64,
}

impl ReadCursor {
    /// Checked construction for a fixed-range read: the next unread byte
    /// must lie at or before the fixed `end_line` bound.
    ///
    /// The fixed end is minted with the cursor and no field access or
    /// setter can move it afterwards, so later output can never grow an
    /// already minted page.
    pub fn new(
        log: LogIdentity,
        next: HistoryPosition,
        end_line: u64,
    ) -> Result<Self, PositionError> {
        if next.line() > end_line {
            return Err(PositionError::NextBeyondEnd {
                next_line: next.line(),
                end_line,
            });
        }
        Ok(Self {
            log,
            next,
            end_line,
        })
    }

    /// Log this cursor was minted from.
    pub fn log(&self) -> &LogIdentity {
        &self.log
    }

    /// Position of the next unread byte.
    pub fn next(&self) -> HistoryPosition {
        self.next
    }

    /// Upper line bound fixed when the read started; later output never
    /// extends it.
    pub fn end_line(&self) -> u64 {
        self.end_line
    }

    /// Whether this cursor is valid for the given log (exact session,
    /// terminal, and epoch match).
    ///
    /// The server maps a mismatch to `CURSOR_EXPIRED`; a cursor is never
    /// silently re-anchored onto another epoch or terminal.
    pub fn belongs_to(&self, log: &LogIdentity) -> bool {
        &self.log == log
    }
}

/// Continuation cursor for live observation (tail follow).
///
/// Unlike [`ReadCursor`] there is no fixed end. `tail_seen` anchors the
/// mutable tail (absent until a tail has been observed), and
/// `state_revision` advances when process/output state changes even
/// without new history lines, so lifecycle updates are never skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCursor {
    /// Log this cursor was minted from.
    log: LogIdentity,
    /// Position of the next unread history byte.
    next_history: HistoryPosition,
    /// Last applied tail position, if a tail has been observed yet.
    tail_seen: Option<TailPosition>,
    /// Revision of the process/output state already observed.
    state_revision: u64,
}

impl ObservationCursor {
    /// Construction for live observation: there is no fixed end to bound,
    /// and `next_history` is already checked by [`HistoryPosition::new`].
    pub fn new(
        log: LogIdentity,
        next_history: HistoryPosition,
        tail_seen: Option<TailPosition>,
        state_revision: u64,
    ) -> Self {
        Self {
            log,
            next_history,
            tail_seen,
            state_revision,
        }
    }

    /// Log this cursor was minted from.
    pub fn log(&self) -> &LogIdentity {
        &self.log
    }

    /// Position of the next unread history byte.
    pub fn next_history(&self) -> HistoryPosition {
        self.next_history
    }

    /// Last applied tail position, if a tail has been observed yet.
    pub fn tail_seen(&self) -> Option<&TailPosition> {
        self.tail_seen.as_ref()
    }

    /// Revision of the process/output state already observed.
    pub fn state_revision(&self) -> u64 {
        self.state_revision
    }

    /// Whether this cursor is valid for the given log (exact session,
    /// terminal, and epoch match).
    ///
    /// The server maps a mismatch to `CURSOR_EXPIRED`; a cursor is never
    /// silently re-anchored onto another epoch or terminal.
    pub fn belongs_to(&self, log: &LogIdentity) -> bool {
        &self.log == log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{
        ExternalSessionId, LogEpoch, SessionRef, SessionSource, TerminalId, TerminalRef,
    };

    fn log(source: &str, external_id: &str, terminal_id: &str, epoch: &str) -> LogIdentity {
        LogIdentity {
            terminal: TerminalRef {
                session: SessionRef {
                    source: SessionSource::new(source),
                    external_id: ExternalSessionId::new(external_id),
                },
                terminal_id: TerminalId::new(terminal_id),
            },
            log_epoch: LogEpoch::new(epoch),
        }
    }

    fn pos(line: u64, byte_offset: u64) -> HistoryPosition {
        HistoryPosition::new(line, byte_offset).expect("valid test position")
    }

    #[test]
    fn cursor_is_bound_to_log_identity() {
        let cursor_log = log("pi", "s1", "t1", "epoch-1");
        let read =
            ReadCursor::new(cursor_log.clone(), pos(3, 0), 10).expect("3 <= 10 is a valid bound");
        let observe = ObservationCursor::new(cursor_log.clone(), pos(3, 0), None, 7);

        // Exact identity (session, terminal, and epoch) matches.
        assert!(read.belongs_to(&cursor_log));
        assert!(observe.belongs_to(&cursor_log));

        // A different epoch (destructive rebuild) invalidates the cursor.
        assert!(!read.belongs_to(&log("pi", "s1", "t1", "epoch-2")));
        assert!(!observe.belongs_to(&log("pi", "s1", "t1", "epoch-2")));

        // Cross-terminal and cross-session reuse is rejected, never
        // silently re-anchored.
        assert!(!read.belongs_to(&log("pi", "s1", "t2", "epoch-1")));
        assert!(!observe.belongs_to(&log("pi", "s1", "t2", "epoch-1")));
        assert!(!read.belongs_to(&log("pi", "s2", "t1", "epoch-1")));
        assert!(!observe.belongs_to(&log("other", "s1", "t1", "epoch-1")));
    }

    #[test]
    fn read_cursor_pins_end_line_observation_cursor_does_not() {
        // Read: the fixed end bound is part of the cursor; later output
        // never extends the page.
        let read = ReadCursor::new(log("pi", "s1", "t1", "epoch-1"), pos(40, 0), 42)
            .expect("40 <= 42 is a valid bound");
        assert_eq!(read.end_line(), 42);

        // Observe: live continuation with no implicit upper bound; the
        // mutable tail and state changes are tracked separately.
        let fresh = ObservationCursor::new(log("pi", "s1", "t1", "epoch-1"), pos(40, 0), None, 0);
        let following = ObservationCursor::new(
            log("pi", "s1", "t1", "epoch-1"),
            pos(40, 0),
            Some(TailPosition::new(TailId::new("tail"), 3, 12)),
            5,
        );
        assert_eq!(fresh.tail_seen(), None);
        assert_eq!(
            following.tail_seen(),
            Some(&TailPosition::new(TailId::new("tail"), 3, 12))
        );
        assert_eq!(following.state_revision(), 5);
    }

    #[test]
    fn history_range_keeps_earliest_in_line_offset() {
        // When the prefix of an over-long line was pruned, the earliest
        // readable position keeps its in-line offset instead of claiming
        // the line is readable from byte 0.
        let range = HistoryRange::new(pos(9, 4096), pos(100, 12))
            .expect("line 9 before line 100 is a valid range");
        assert_eq!(range.earliest(), pos(9, 4096));
        assert_eq!(range.latest(), pos(100, 12));
    }

    #[test]
    fn history_positions_are_one_based() {
        // There is no line 0, with or without an in-line offset.
        assert_eq!(
            HistoryPosition::new(0, 0),
            Err(PositionError::ZeroLine { line: 0 })
        );
        assert_eq!(
            HistoryPosition::new(0, 4096),
            Err(PositionError::ZeroLine { line: 0 })
        );

        // Line 1 with any in-line offset is valid.
        assert_eq!(pos(1, 0).line(), 1);
        assert_eq!(pos(1, 4096).byte_offset(), 4096);
    }

    #[test]
    fn read_cursor_rejects_next_line_beyond_fixed_end() {
        let cursor_log = log("pi", "s1", "t1", "epoch-1");

        // The next unread line must not lie beyond the fixed end.
        assert_eq!(
            ReadCursor::new(cursor_log.clone(), pos(11, 0), 10),
            Err(PositionError::NextBeyondEnd {
                next_line: 11,
                end_line: 10
            })
        );

        // A zero end bound can never hold a 1-based next line.
        assert_eq!(
            ReadCursor::new(cursor_log.clone(), pos(1, 0), 0),
            Err(PositionError::NextBeyondEnd {
                next_line: 1,
                end_line: 0
            })
        );

        // Reading up to and including the end line itself is fine.
        let at_end = ReadCursor::new(cursor_log, pos(10, 4), 10)
            .expect("next on the end line is a valid bound");
        assert_eq!(at_end.next().line(), at_end.end_line());
    }

    #[test]
    fn history_range_rejects_reversed_bounds() {
        // Same line, earliest offset after latest offset.
        assert_eq!(
            HistoryRange::new(pos(5, 10), pos(5, 9)),
            Err(PositionError::ReversedRange {
                earliest: pos(5, 10),
                latest: pos(5, 9)
            })
        );

        // Strictly later earliest line, regardless of offsets.
        assert_eq!(
            HistoryRange::new(pos(6, 0), pos(5, 4096)),
            Err(PositionError::ReversedRange {
                earliest: pos(6, 0),
                latest: pos(5, 4096)
            })
        );

        // A degenerate single-position range is valid.
        assert!(HistoryRange::new(pos(5, 10), pos(5, 10)).is_ok());
    }

    #[test]
    fn read_cursor_fixed_end_cannot_be_extended() {
        // The fixed end is minted with the cursor and its fields are
        // private with no setter, so nothing can move it afterwards;
        // later output therefore never grows an already minted page.
        let read = ReadCursor::new(log("pi", "s1", "t1", "epoch-1"), pos(40, 0), 42)
            .expect("40 <= 42 is a valid bound");
        assert_eq!(read.end_line(), 42);
        assert_eq!(read.next(), pos(40, 0));
        assert!(read.belongs_to(read.log()));
    }
}
