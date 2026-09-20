//! Stable reason categories for terminal failures.

/// Machine-readable reason category for terminal failures.
///
/// These are wire-agnostic domain categories, not protocol values: the
/// daemon's gRPC adapter maps them onto the generated error-detail enum
/// once the production `.proto` freezes, and any string or numeric
/// encoding belongs to that adapter, not here. Membership may still grow
/// before the freeze, so the enum is marked `#[non_exhaustive]`: matching
/// code outside this crate must keep a wildcard arm instead of breaking
/// when a category is added.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    /// Another control client holds the session lease.
    ControlBusy,
    /// The control token expired or was released.
    ControlExpired,
    /// A history, tail, or scan cursor is no longer valid for its log.
    CursorExpired,
    /// The terminal no longer accepts input.
    TerminalNotWritable,
    /// A write was aborted after some bytes were written; the known byte
    /// count travels in typed error details, not here.
    PartialWrite,
}
