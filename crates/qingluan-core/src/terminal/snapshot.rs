//! Terminal snapshot types: independent process and output states.

use super::ids::TerminalRef;
use super::position::HistoryRange;

/// Terminal window size.
///
/// The default size is a daemon/config concern and is deliberately not
/// encoded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    /// Number of rows.
    pub rows: u16,
    /// Number of columns.
    pub columns: u16,
}

/// Known outcome of the root process.
///
/// Filled only when actually known; an unknown outcome is
/// [`ProcessState::Interrupted`], never a sentinel exit code or signal.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// The root process is running.
    Running,
    /// The root process exited with a known result.
    Exited(ExitResult),
    /// No trustworthy exit result exists (for example an old record found
    /// after a daemon restart).
    Interrupted,
}

/// Raw OS exit outcome.
///
/// Values stay raw (no libc/nix types, no normalization, no sentinel).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitResult {
    /// The process exited by itself with this exit code.
    ExitCode(i32),
    /// The process was terminated by this OS signal.
    Signal(i32),
}

/// State of the output-read side of the terminal.
///
/// Independent of [`ProcessState`]: the root process may have exited while
/// output is still open, and output may close while the process still
/// runs. There is no guaranteed ordering between the two.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputState {
    /// Output is still being read.
    Open,
    /// Output reading has ended; the variant records how.
    Closed(OutputEnd),
}

/// How output reading ended.
///
/// `Eof` means a normal end (including master EIO after the last slave
/// closes). Every other variant means the tail may be incomplete and must
/// not be presented as a normal EOF. Finer reason payloads for
/// `ForcedClose`/`ReadError` are classified in the PTY slice; adding them
/// later is an accepted pre-freeze domain change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputEnd {
    /// Normal end of output.
    Eof,
    /// Closed forcibly (for example a stop's bounded output close timing
    /// out).
    ForcedClose,
    /// Reading failed; the output may be incomplete.
    ReadError,
    /// No trustworthy output end exists (for example after a daemon
    /// restart).
    Interrupted,
}

/// Point-in-time state of one terminal.
///
/// `process`, `output`, and `stopping` are three independent dimensions:
/// `ProcessExited` and `OutputClosed` have no guaranteed order, so no
/// single merged running/exited enum can represent all resource states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    /// Which terminal this snapshot describes.
    pub terminal: TerminalRef,
    /// Root process state.
    pub process: ProcessState,
    /// Output-read state.
    pub output: OutputState,
    /// A stop flow has been committed and is progressing.
    pub stopping: bool,
    /// Current window size.
    pub size: TerminalSize,
    /// History currently retained (readable) by storage; absent before
    /// the first history line is committed or after all history is cleared.
    pub retained_history: Option<HistoryRange>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{
        ExternalSessionId, HistoryPosition, SessionRef, SessionSource, TerminalId,
    };

    fn terminal_ref() -> TerminalRef {
        TerminalRef {
            session: SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("s1"),
            },
            terminal_id: TerminalId::new("t1"),
        }
    }

    fn retained_history() -> HistoryRange {
        let earliest = HistoryPosition::new(1, 0).expect("line 1 is valid");
        let latest = HistoryPosition::new(5, 0).expect("line 5 is valid");
        HistoryRange::new(earliest, latest).expect("line 1 before line 5 is a valid range")
    }

    fn snapshot(process: ProcessState, output: OutputState, stopping: bool) -> TerminalSnapshot {
        TerminalSnapshot {
            terminal: terminal_ref(),
            process,
            output,
            stopping,
            size: TerminalSize {
                rows: 30,
                columns: 120,
            },
            retained_history: Some(retained_history()),
        }
    }

    #[test]
    fn process_and_output_states_are_independent() {
        // Root exited while output stays open (a child still holds the
        // PTY).
        let exited_open = snapshot(
            ProcessState::Exited(ExitResult::ExitCode(0)),
            OutputState::Open,
            false,
        );
        assert_eq!(
            exited_open.process,
            ProcessState::Exited(ExitResult::ExitCode(0))
        );
        assert_eq!(exited_open.output, OutputState::Open);
        assert_eq!(exited_open.retained_history, Some(retained_history()));

        // Output closed normally while the root process still runs.
        let running_closed = snapshot(
            ProcessState::Running,
            OutputState::Closed(OutputEnd::Eof),
            false,
        );
        assert_eq!(running_closed.process, ProcessState::Running);
        assert_eq!(running_closed.output, OutputState::Closed(OutputEnd::Eof));

        // Stop committed after exit with the PTY still open: forced close
        // and stopping set.
        let stopped_after_exit = snapshot(
            ProcessState::Exited(ExitResult::Signal(9)),
            OutputState::Closed(OutputEnd::ForcedClose),
            true,
        );
        assert_eq!(
            stopped_after_exit.process,
            ProcessState::Exited(ExitResult::Signal(9))
        );
        assert_eq!(
            stopped_after_exit.output,
            OutputState::Closed(OutputEnd::ForcedClose)
        );
        assert!(stopped_after_exit.stopping);

        // Daemon-restart leftovers without trustworthy outcomes.
        let interrupted = snapshot(
            ProcessState::Interrupted,
            OutputState::Closed(OutputEnd::Interrupted),
            false,
        );
        assert_eq!(interrupted.process, ProcessState::Interrupted);
        assert_eq!(
            interrupted.output,
            OutputState::Closed(OutputEnd::Interrupted)
        );

        // `stopping` toggles independently of both dimensions.
        let mut stopping = exited_open.clone();
        stopping.stopping = true;
        assert!(stopping.stopping);
        assert_eq!(stopping.process, exited_open.process);
        assert_eq!(stopping.output, exited_open.output);
        assert_ne!(stopping, exited_open);

        // A newly started terminal can have no committed history yet.
        let mut empty = snapshot(ProcessState::Running, OutputState::Open, false);
        empty.retained_history = None;
        assert_eq!(empty.retained_history, None);
    }

    #[test]
    fn interrupted_is_not_a_sentinel_exit_result() {
        // Unknown outcomes are the dedicated variant, never a fabricated
        // ExitCode(-1)/Signal(-1) standing in for "unknown".
        assert_ne!(
            ProcessState::Interrupted,
            ProcessState::Exited(ExitResult::ExitCode(-1))
        );
        assert_ne!(
            ProcessState::Interrupted,
            ProcessState::Exited(ExitResult::Signal(-1))
        );
    }
}
