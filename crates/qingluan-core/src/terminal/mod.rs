//! Terminal domain types shared by `qingluan-terminal`, `qingluan-storage`,
//! and the `qingluan-daemon` conversion layer.
//!
//! Scope (deep module): pure value types and checked rules for terminal
//! and log identity, history positions and cursors, session lifecycle
//! events and their watermarks, terminal snapshots, and failure reason
//! codes. No PTY, cgroup, SQLite, or gRPC concept appears here. The types
//! carry no serde derives on purpose: wire shapes belong to
//! `qingluan-protocol` and storage persistence is internal to
//! `qingluan-storage`, so deriving a serialized shape here would freeze
//! one needlessly — a deliberate divergence from the serialized
//! `crate::workspace` catalog types.
//!
//! Field names may still change until the production `.proto` is frozen;
//! the invariants below are the stable contract:
//!
//! - Event watermarks are cumulative bounds with
//!   `pruned_through_seq <= acked_through_seq <= last_committed_seq`, all
//!   starting at zero; [`SessionEventState::new`] rejects inversions.
//! - Positions and cursors bind to an exact [`LogIdentity`] (session,
//!   terminal, and log epoch); a cursor minted against another epoch or
//!   terminal must be rejected, never silently re-anchored.
//! - History line numbers start at 1 and are never reused; `byte_offset`
//!   is only meaningful inside a line. [`HistoryPosition::new`] rejects
//!   line zero, and cursor/range constructors reject reversed bounds;
//!   a fixed-range cursor's end is minted with it and never extends.
//! - Process and output state are independent snapshot dimensions with no
//!   guaranteed ordering between them; an unknown process outcome is
//!   [`ProcessState::Interrupted`], never a sentinel exit code or signal.
//! - Reason categories and lifecycle event payloads are wire-agnostic
//!   `#[non_exhaustive]` enums until the production `.proto` freezes:
//!   protocol encodings belong to the daemon adapter, and adding a
//!   variant must not break matching code outside this crate.
//! - A start is fully explicit: program, args, cwd, a complete UTF-8
//!   environment snapshot (empty is legal, missing is unrepresentable),
//!   and the initial size. A control lease is identified by a non-zero
//!   generation; a send that stopped early is a typed partial write
//!   carrying its exact known byte count, classified by a wire-agnostic
//!   abort reason.
//! - Query values are checked before they are issued: read limits are
//!   non-zero and clamped to their hard caps, a complete read page cannot
//!   carry a continuation, a grep scan point is bound to the exact query
//!   it came from, and a partial grep scan never claims completeness. A
//!   degraded log is reported through the page instead of presenting an
//!   unlocated loss as continuous history.

mod control;
mod event;
mod ids;
mod position;
mod query;
mod reason;
mod send;
mod snapshot;
mod start;

pub use control::ControlGeneration;
pub use event::{
    EventPage, EventSequence, SessionEvent, SessionEventPayload, SessionEventState, WatermarkError,
};
pub use ids::{
    ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TailId, TerminalId,
    TerminalRef,
};
pub use position::{
    HistoryPosition, HistoryRange, ObservationCursor, PositionError, ReadCursor, TailPosition,
};
pub use query::{
    GrepContext, GrepLimits, GrepMatch, GrepPage, GrepQuery, GrepRequest, GrepScanPoint, GrepStop,
    LimitError, LineFragment, QueryError, ReadLimits, ReadPage, ReadRequest, ReadResult, ReadStart,
    ReadTruncation, TailSnapshot, TailView,
};
pub use reason::ReasonCode;
pub use send::{PartialWrite, SendReceipt, WriteAbort};
pub use snapshot::{
    ExitResult, OutputEnd, OutputState, ProcessState, TerminalSize, TerminalSnapshot,
};
pub use start::{EnvironmentSnapshot, StartSpec};
