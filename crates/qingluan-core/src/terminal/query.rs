//! Query value types: fixed-range reads, the mutable tail, and literal
//! grep.
//!
//! These are the wire-agnostic shapes the storage and execution seams
//! speak, with the invariants that must hold before a query is issued or
//! a response is handed back:
//!
//! - A **fixed-range read** pins its upper line bound when it starts
//!   ([`ReadCursor`]`::end_line`); later output never extends an already
//!   minted page. [`ReadRequest`] carries either a start position or a
//!   continuation cursor (never both), and a cursor must belong to the
//!   request's exact [`LogIdentity`].
//! - A **page** that reports no truncation is complete and therefore has
//!   no continuation ([`ReadPage::new`] rejects the contradiction).
//! - The **mutable tail** is addressed by an independent
//!   [`TailPosition`] (tail id + revision + byte offset); overwriting
//!   invalidates older revisions, and a bounded tail reports an omitted
//!   prefix through [`TailSnapshot::truncated`] instead of silently
//!   dropping content.
//! - **Literal grep** binds its continuation to the exact query it came
//!   from ([`GrepScanPoint`]): a changed needle, case option, context
//!   option, log, or fixed range cannot reuse an old scan position, and a
//!   partial scan never claims completeness ([`GrepPage::new`]).
//! - Budgets are explicit: reads default to 200 lines / 32 KiB with hard
//!   caps of 1000 lines / 256 KiB, and grep keeps a response budget
//!   (`max_matches` / `max_bytes`) separate from its scan budget
//!   (`scan_bytes`).
//! - A degraded log is reported as such: pages carry
//!   [`ReadPage::degraded`] / [`GrepPage::degraded`] so an unlocated loss
//!   is never presented as continuous history.

use super::ids::LogIdentity;
use super::position::{HistoryPosition, HistoryRange, ReadCursor, TailPosition};

/// A query value violates a documented invariant, or a query cannot be
/// served from the requested position.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// The requested position is no longer readable. `missing` is the
    /// exact range that is gone when it is known (a recorded gap, or the
    /// range between the requested position and the current floor);
    /// `earliest` is the earliest position that is still readable, if any
    /// is. Recovery is the caller's explicit choice: a refused position is
    /// never silently re-anchored.
    #[error("query position is no longer readable: earliest {earliest:?}, missing {missing:?}")]
    CursorExpired {
        /// Earliest position still readable, if any.
        earliest: Option<HistoryPosition>,
        /// Exact missing range, when it is known.
        missing: Option<HistoryRange>,
    },
    /// The requested range intersects an explicitly recorded gap.
    #[error("history is missing in {range:?}")]
    Gap {
        /// The missing range.
        range: HistoryRange,
    },
    /// The request itself is inconsistent (for example both a start and a
    /// cursor, a cursor minted against another log, an empty needle, or a
    /// continuation point from a different query).
    #[error("invalid query: {detail}")]
    Invalid {
        /// Human-readable detail.
        detail: String,
    },
}

impl QueryError {
    /// Whether this refusal means a position was lost (rather than a
    /// malformed request).
    pub fn is_expired(&self) -> bool {
        matches!(
            self,
            QueryError::CursorExpired { .. } | QueryError::Gap { .. }
        )
    }
}

fn invalid(detail: impl Into<String>) -> QueryError {
    QueryError::Invalid {
        detail: detail.into(),
    }
}

/// A limit value that cannot make progress or exceeds its hard cap.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LimitError {
    /// A budget of zero can never return anything; reject it instead of
    /// letting a query stop before it started.
    #[error("{field} budget must be non-zero")]
    Zero {
        /// Which budget was zero.
        field: &'static str,
    },
}

/// Fixed-range read limits.
///
/// Defaults are 200 lines / 32 KiB; the server never returns more than
/// the hard caps (1000 lines / 256 KiB) regardless of what a caller
/// requests, so one page's allocation is bounded by a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    max_lines: u32,
    max_bytes: u32,
}

impl ReadLimits {
    /// Default line budget of one page.
    pub const DEFAULT_MAX_LINES: u32 = 200;
    /// Default byte budget of one page.
    pub const DEFAULT_MAX_BYTES: u32 = 32 * 1024;
    /// Hard cap on the line budget of one page.
    pub const HARD_MAX_LINES: u32 = 1000;
    /// Hard cap on the byte budget of one page.
    pub const HARD_MAX_BYTES: u32 = 256 * 1024;
    /// The default read limits.
    pub const DEFAULT: ReadLimits = ReadLimits {
        max_lines: Self::DEFAULT_MAX_LINES,
        max_bytes: Self::DEFAULT_MAX_BYTES,
    };
    /// The hard caps: the largest page this server will ever construct.
    pub const HARD_CAP: ReadLimits = ReadLimits {
        max_lines: Self::HARD_MAX_LINES,
        max_bytes: Self::HARD_MAX_BYTES,
    };

    /// Checked construction: both budgets must be non-zero. Values above
    /// the hard caps are accepted and clamped by [`ReadLimits::clamped`],
    /// so a caller cannot ask for an unbounded page even by mistake.
    pub fn new(max_lines: u32, max_bytes: u32) -> Result<Self, LimitError> {
        if max_lines == 0 {
            return Err(LimitError::Zero { field: "max_lines" });
        }
        if max_bytes == 0 {
            return Err(LimitError::Zero { field: "max_bytes" });
        }
        Ok(Self {
            max_lines,
            max_bytes,
        })
    }

    /// These limits clamped to the hard caps.
    pub fn clamped(self) -> Self {
        Self {
            max_lines: self.max_lines.min(Self::HARD_MAX_LINES),
            max_bytes: self.max_bytes.min(Self::HARD_MAX_BYTES),
        }
    }

    /// Line budget of one page.
    pub fn max_lines(&self) -> u32 {
        self.max_lines
    }

    /// Byte budget of one page.
    pub fn max_bytes(&self) -> u32 {
        self.max_bytes
    }
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One returned fragment of a normalized history line.
///
/// `text` is always a whole-UTF-8-character slice of the line, so a
/// fragment boundary is always a rune boundary. `position.byte_offset` is
/// the offset of `text`'s first byte inside the line, `prefix_omitted`
/// says the fragment does not start where the line starts (the requested
/// offset was clamped forward, or the line's prefix was pruned), and
/// `suffix_remaining` says more of this line follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFragment {
    position: HistoryPosition,
    text: String,
    prefix_omitted: bool,
    suffix_remaining: bool,
}

impl LineFragment {
    /// Construction from an already-rune-aligned slice of one line.
    pub fn new(
        position: HistoryPosition,
        text: impl Into<String>,
        prefix_omitted: bool,
        suffix_remaining: bool,
    ) -> Self {
        Self {
            position,
            text: text.into(),
            prefix_omitted,
            suffix_remaining,
        }
    }

    /// Position of this fragment's first byte inside its line.
    pub fn position(&self) -> HistoryPosition {
        self.position
    }

    /// The fragment text (whole characters only).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the fragment starts after the line's first byte.
    pub fn prefix_omitted(&self) -> bool {
        self.prefix_omitted
    }

    /// Whether more of this line remains after the fragment.
    pub fn suffix_remaining(&self) -> bool {
        self.suffix_remaining
    }
}

/// Why a read page stopped before its fixed upper bound.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTruncation {
    /// The page's byte budget was reached.
    ByteBudget,
    /// The page's line budget was reached.
    LineBudget,
    /// The fixed range is interrupted by an explicitly recorded gap; the
    /// page ends before it and its continuation is dropped rather than
    /// stitched across the hole.
    Gap,
}

/// One page of a fixed-range read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPage {
    fragments: Vec<LineFragment>,
    next: Option<ReadCursor>,
    truncation: Option<ReadTruncation>,
    retained: Option<HistoryRange>,
    degraded: bool,
}

impl ReadPage {
    /// Checked construction. A page that reports no truncation is
    /// complete, so it cannot also carry a continuation: a `next` cursor
    /// without a `truncation` reason would let a caller keep reading past
    /// a page that claimed to be finished. A truncation reason may stand
    /// without a continuation (a page that ends at a recorded gap is not
    /// resumable from that position).
    pub fn new(
        fragments: Vec<LineFragment>,
        next: Option<ReadCursor>,
        truncation: Option<ReadTruncation>,
        retained: Option<HistoryRange>,
        degraded: bool,
    ) -> Result<Self, QueryError> {
        if truncation.is_none() && next.is_some() {
            return Err(invalid(
                "a complete page (no truncation) cannot carry a continuation cursor",
            ));
        }
        Ok(Self {
            fragments,
            next,
            truncation,
            retained,
            degraded,
        })
    }

    /// The returned fragments, in line order.
    pub fn fragments(&self) -> &[LineFragment] {
        &self.fragments
    }

    /// The continuation cursor, or `None` when the fixed range is
    /// exhausted (or the page ended at a gap).
    pub fn next(&self) -> Option<&ReadCursor> {
        self.next.as_ref()
    }

    /// Why the page stopped before its fixed bound, if it did.
    pub fn truncation(&self) -> Option<ReadTruncation> {
        self.truncation
    }

    /// Currently retained (readable) span of this log, if any.
    pub fn retained(&self) -> Option<HistoryRange> {
        self.retained
    }

    /// Whether the log carries an explicit loss, so this page's
    /// completeness cannot be guaranteed even though it was served
    /// without error. A page never presents an unlocated loss as
    /// continuous history.
    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

/// Where the first page of a read starts.
///
/// This is a closed set of start intentions rather than an optional
/// position, so "no position given" can never be confused with "no history
/// to read" (which is a served, empty page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStart {
    /// From the earliest readable position.
    Earliest,
    /// From an explicit position.
    At(HistoryPosition),
    /// The newest lines that fit the line budget: the server picks the
    /// window (never starting inside an explicit gap), so a caller can ask
    /// for recent history without knowing where it begins.
    Newest,
}

/// One fixed-range read request: either the first page (from a
/// [`ReadStart`]) or a continuation of a cursor minted earlier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    log: LogIdentity,
    start: ReadStart,
    cursor: Option<ReadCursor>,
    limits: ReadLimits,
}

impl ReadRequest {
    /// Checked construction: a request carries a start intention or a
    /// continuation cursor, never both, and a continuation cursor must
    /// belong to exactly this log (session, terminal, and epoch).
    pub fn new(
        log: LogIdentity,
        start: Option<ReadStart>,
        cursor: Option<ReadCursor>,
        limits: ReadLimits,
    ) -> Result<Self, QueryError> {
        if start.is_some() && cursor.is_some() {
            return Err(invalid("a read carries a start or a cursor, not both"));
        }
        if let Some(cursor) = &cursor
            && !cursor.belongs_to(&log)
        {
            return Err(invalid(
                "read cursor does not belong to the requested log identity",
            ));
        }
        Ok(Self {
            log,
            start: start.unwrap_or(ReadStart::Earliest),
            cursor,
            limits: limits.clamped(),
        })
    }

    /// A first-page read from `start` (the earliest readable position when
    /// none is given).
    pub fn first(
        log: LogIdentity,
        start: Option<ReadStart>,
        limits: ReadLimits,
    ) -> Result<Self, QueryError> {
        Self::new(log, start, None, limits)
    }

    /// A continuation of `cursor` (whose fixed end bound is preserved).
    pub fn resume(cursor: ReadCursor, limits: ReadLimits) -> Self {
        Self {
            log: cursor.log().clone(),
            start: ReadStart::Earliest,
            cursor: Some(cursor),
            limits: limits.clamped(),
        }
    }

    /// Log this request addresses.
    pub fn log(&self) -> &LogIdentity {
        &self.log
    }

    /// Where a first page starts (ignored by a continuation).
    pub fn start(&self) -> ReadStart {
        self.start
    }

    /// Continuation cursor, if this is not a first page.
    pub fn cursor(&self) -> Option<&ReadCursor> {
        self.cursor.as_ref()
    }

    /// Limits clamped to the hard caps.
    pub fn limits(&self) -> ReadLimits {
        self.limits
    }
}

/// One served read page plus the cursor it was read with.
///
/// `cursor` always carries the read's immutable upper bound (`end_line`),
/// including on a page whose `next` is absent, so a client can tell a
/// finished range from a page that simply had nothing new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    cursor: ReadCursor,
    page: ReadPage,
}

impl ReadResult {
    /// Construction from a served page and the cursor it used.
    pub fn new(cursor: ReadCursor, page: ReadPage) -> Self {
        Self { cursor, page }
    }

    /// The cursor this page was read with (fixed `end_line` included).
    pub fn cursor(&self) -> &ReadCursor {
        &self.cursor
    }

    /// The served page.
    pub fn page(&self) -> &ReadPage {
        &self.page
    }
}

/// Snapshot of the mutable, not-yet-finalized tail line.
///
/// `text` is the retained suffix of the logical line;
/// [`TailSnapshot::truncated`] says the prefix was omitted by the tail
/// bound (never silently). `position.byte_offset` is absolute inside the
/// logical line, so a reader can tell where the retained text starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailSnapshot {
    log: LogIdentity,
    position: TailPosition,
    text: String,
    truncated: bool,
}

impl TailSnapshot {
    /// Construction from the tail's current state.
    pub fn new(
        log: LogIdentity,
        position: TailPosition,
        text: impl Into<String>,
        truncated: bool,
    ) -> Self {
        Self {
            log,
            position,
            text: text.into(),
            truncated,
        }
    }

    /// Log this tail belongs to.
    pub fn log(&self) -> &LogIdentity {
        &self.log
    }

    /// Position of this tail revision.
    pub fn position(&self) -> &TailPosition {
        &self.position
    }

    /// The retained tail text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the logical line's prefix was omitted.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// One consistent view of a terminal's committed history and its mutable
/// tail: the newest committed lines within the read budget plus the
/// current tail snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailView {
    history: ReadResult,
    tail: TailSnapshot,
}

impl TailView {
    /// Construction from one consistent sample.
    pub fn new(history: ReadResult, tail: TailSnapshot) -> Self {
        Self { history, tail }
    }

    /// The newest committed history within the read budget.
    pub fn history(&self) -> &ReadResult {
        &self.history
    }

    /// The mutable tail line.
    pub fn tail(&self) -> &TailSnapshot {
        &self.tail
    }
}

/// Literal search over a fixed committed history range, bound to one log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepQuery {
    log: LogIdentity,
    needle: String,
    case_sensitive: bool,
    start: HistoryPosition,
    end_line: u64,
    context_lines: u16,
}

impl GrepQuery {
    /// Largest accepted needle.
    pub const MAX_NEEDLE_BYTES: usize = 4096;
    /// Largest accepted context width.
    pub const MAX_CONTEXT_LINES: u16 = 50;

    /// Checked construction: a non-empty needle within its bound, a fixed
    /// range that starts at or before its end line, and a bounded context
    /// width.
    pub fn new(
        log: LogIdentity,
        needle: impl Into<String>,
        case_sensitive: bool,
        start: HistoryPosition,
        end_line: u64,
        context_lines: u16,
    ) -> Result<Self, QueryError> {
        let needle = needle.into();
        if needle.is_empty() {
            return Err(invalid("grep needle must not be empty"));
        }
        if needle.len() > Self::MAX_NEEDLE_BYTES {
            return Err(invalid("grep needle exceeds its byte bound"));
        }
        if start.line() > end_line {
            return Err(invalid("grep range starts after its fixed end line"));
        }
        if context_lines > Self::MAX_CONTEXT_LINES {
            return Err(invalid("grep context width exceeds its bound"));
        }
        Ok(Self {
            log,
            needle,
            case_sensitive,
            start,
            end_line,
            context_lines,
        })
    }

    /// Log this query addresses.
    pub fn log(&self) -> &LogIdentity {
        &self.log
    }

    /// The literal searched for.
    pub fn needle(&self) -> &str {
        &self.needle
    }

    /// Whether case matters.
    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// First line of the fixed range.
    pub fn start(&self) -> HistoryPosition {
        self.start
    }

    /// Fixed upper line bound of the range.
    pub fn end_line(&self) -> u64 {
        self.end_line
    }

    /// Context lines requested around each match.
    pub fn context_lines(&self) -> u16 {
        self.context_lines
    }
}

/// Grep budgets. The response budget (`max_matches` / `max_bytes`) and
/// the scan budget (`scan_bytes`) are separate on purpose: a page may
/// return zero matches while the range it could examine was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepLimits {
    max_matches: u32,
    max_bytes: u32,
    scan_bytes: u64,
}

impl GrepLimits {
    /// Default match budget of one page.
    pub const DEFAULT_MAX_MATCHES: u32 = 200;
    /// Default response byte budget of one page.
    pub const DEFAULT_MAX_BYTES: u32 = 32 * 1024;
    /// Default scanned-byte budget of one page.
    pub const DEFAULT_SCAN_BYTES: u64 = 4 * 1024 * 1024;
    /// Hard cap on the match budget.
    pub const HARD_MAX_MATCHES: u32 = 1000;
    /// Hard cap on the response byte budget.
    pub const HARD_MAX_BYTES: u32 = 256 * 1024;
    /// Hard cap on the scanned-byte budget.
    pub const HARD_MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
    /// The default grep budgets.
    pub const DEFAULT: GrepLimits = GrepLimits {
        max_matches: Self::DEFAULT_MAX_MATCHES,
        max_bytes: Self::DEFAULT_MAX_BYTES,
        scan_bytes: Self::DEFAULT_SCAN_BYTES,
    };
    /// The hard caps.
    pub const HARD_CAP: GrepLimits = GrepLimits {
        max_matches: Self::HARD_MAX_MATCHES,
        max_bytes: Self::HARD_MAX_BYTES,
        scan_bytes: Self::HARD_MAX_SCAN_BYTES,
    };

    /// Checked construction: every budget must be non-zero (a zero budget
    /// cannot make progress).
    pub fn new(max_matches: u32, max_bytes: u32, scan_bytes: u64) -> Result<Self, LimitError> {
        if max_matches == 0 {
            return Err(LimitError::Zero {
                field: "max_matches",
            });
        }
        if max_bytes == 0 {
            return Err(LimitError::Zero { field: "max_bytes" });
        }
        if scan_bytes == 0 {
            return Err(LimitError::Zero {
                field: "scan_bytes",
            });
        }
        Ok(Self {
            max_matches,
            max_bytes,
            scan_bytes,
        })
    }

    /// These budgets clamped to the hard caps.
    pub fn clamped(self) -> Self {
        Self {
            max_matches: self.max_matches.min(Self::HARD_MAX_MATCHES),
            max_bytes: self.max_bytes.min(Self::HARD_MAX_BYTES),
            scan_bytes: self.scan_bytes.min(Self::HARD_MAX_SCAN_BYTES),
        }
    }

    /// Match budget of one page.
    pub fn max_matches(&self) -> u32 {
        self.max_matches
    }

    /// Response byte budget of one page.
    pub fn max_bytes(&self) -> u32 {
        self.max_bytes
    }

    /// Scanned-byte budget of one page.
    pub fn scan_bytes(&self) -> u64 {
        self.scan_bytes
    }
}

impl Default for GrepLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One literal match inside a history line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepMatch {
    position: HistoryPosition,
    length: u64,
}

impl GrepMatch {
    /// Checked construction: a match has at least one byte.
    pub fn new(position: HistoryPosition, length: u64) -> Result<Self, QueryError> {
        if length == 0 {
            return Err(invalid("a grep match must have a non-zero length"));
        }
        Ok(Self { position, length })
    }

    /// Position of the match's first byte.
    pub fn position(&self) -> HistoryPosition {
        self.position
    }

    /// Matched length in bytes.
    pub fn length(&self) -> u64 {
        self.length
    }
}

/// One context line returned around a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepContext {
    line: u64,
    text: String,
    truncated: bool,
}

impl GrepContext {
    /// Checked construction: context lines are 1-based like every other
    /// history position.
    pub fn new(line: u64, text: impl Into<String>, truncated: bool) -> Result<Self, QueryError> {
        HistoryPosition::new(line, 0).map_err(|_| invalid("context line numbers start at 1"))?;
        Ok(Self {
            line,
            text: text.into(),
            truncated,
        })
    }

    /// 1-based line number of the context line.
    pub fn line(&self) -> u64 {
        self.line
    }

    /// The context text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this context line was shortened by the response budget.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Continuation point of one literal scan.
///
/// The point carries the exact query it came from, so changing the
/// needle, the case or context option, the log, or the fixed range cannot
/// reuse an old scan position: [`GrepScanPoint::matches`] compares the
/// complete query by value, and a resuming request must repeat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepScanPoint {
    query: GrepQuery,
    next: HistoryPosition,
}

impl GrepScanPoint {
    /// Checked construction: the continuation position lies inside the
    /// query's fixed range.
    pub fn new(query: GrepQuery, next: HistoryPosition) -> Result<Self, QueryError> {
        if next.line() < query.start().line() || next.line() > query.end_line() {
            return Err(invalid("scan point lies outside its query's fixed range"));
        }
        Ok(Self { query, next })
    }

    /// The query this point is bound to.
    pub fn query(&self) -> &GrepQuery {
        &self.query
    }

    /// Position of the next unscanned byte.
    pub fn next(&self) -> HistoryPosition {
        self.next
    }

    /// Whether this point belongs to exactly this query.
    pub fn matches(&self, query: &GrepQuery) -> bool {
        &self.query == query
    }
}

/// One literal grep request: a fresh query, or a query resumed at a point
/// minted by an earlier page of the same query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepRequest {
    query: GrepQuery,
    resume: Option<GrepScanPoint>,
}

impl GrepRequest {
    /// Checked construction: a resuming request must repeat the exact
    /// query its point was minted from.
    pub fn new(query: GrepQuery, resume: Option<GrepScanPoint>) -> Result<Self, QueryError> {
        if let Some(point) = &resume
            && !point.matches(&query)
        {
            return Err(invalid(
                "scan point does not belong to this query; changing the needle, options, \
                 log, or range cannot continue an old scan",
            ));
        }
        Ok(Self { query, resume })
    }

    /// A fresh scan of `query`.
    pub fn fresh(query: GrepQuery) -> Self {
        Self {
            query,
            resume: None,
        }
    }

    /// The bound query.
    pub fn query(&self) -> &GrepQuery {
        &self.query
    }

    /// The continuation point, if this is not the first page.
    pub fn resume(&self) -> Option<&GrepScanPoint> {
        self.resume.as_ref()
    }
}

/// Why the scan of a page stopped.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepStop {
    /// The fixed range was scanned to its end.
    RangeExhausted,
    /// The scanned-byte budget was reached.
    ScanBudget,
    /// The response budget (matches or bytes) was reached.
    ResponseBudget,
}

/// One page of a literal grep scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepPage {
    matches: Vec<GrepMatch>,
    contexts: Vec<GrepContext>,
    scanned_range: HistoryRange,
    next_scan: Option<GrepScanPoint>,
    stopped: GrepStop,
    contexts_truncated: bool,
    degraded: bool,
}

impl GrepPage {
    /// Checked construction: only a page that scanned its whole fixed
    /// range may report [`GrepStop::RangeExhausted`] (and then carries no
    /// continuation); every other stop carries the point to resume at, so
    /// a partial scan can never claim completeness — not even with zero
    /// matches.
    pub fn new(
        matches: Vec<GrepMatch>,
        contexts: Vec<GrepContext>,
        scanned_range: HistoryRange,
        next_scan: Option<GrepScanPoint>,
        stopped: GrepStop,
        contexts_truncated: bool,
        degraded: bool,
    ) -> Result<Self, QueryError> {
        let complete = stopped == GrepStop::RangeExhausted;
        if complete == next_scan.is_some() {
            return Err(invalid(
                "a grep page carries a continuation exactly when its scan did not \
                 exhaust the fixed range",
            ));
        }
        Ok(Self {
            matches,
            contexts,
            scanned_range,
            next_scan,
            stopped,
            contexts_truncated,
            degraded,
        })
    }

    /// Matches found in this page, in line and offset order.
    pub fn matches(&self) -> &[GrepMatch] {
        &self.matches
    }

    /// Context lines around the matches in this page.
    pub fn contexts(&self) -> &[GrepContext] {
        &self.contexts
    }

    /// The part of the fixed range this page actually examined.
    pub fn scanned_range(&self) -> HistoryRange {
        self.scanned_range
    }

    /// The continuation point, absent only when the whole range was
    /// scanned.
    pub fn next_scan(&self) -> Option<&GrepScanPoint> {
        self.next_scan.as_ref()
    }

    /// Why the scan stopped.
    pub fn stopped(&self) -> GrepStop {
        self.stopped
    }

    /// Whether the response budget shortened a context line.
    pub fn contexts_truncated(&self) -> bool {
        self.contexts_truncated
    }

    /// Whether the log carries an explicit loss, so completeness cannot be
    /// guaranteed even for a fully scanned range.
    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{
        ExternalSessionId, LogEpoch, LogIdentity, SessionRef, SessionSource, TailId, TerminalId,
        TerminalRef,
    };

    fn log(terminal_id: &str) -> LogIdentity {
        LogIdentity {
            terminal: TerminalRef {
                session: SessionRef {
                    source: SessionSource::new("pi"),
                    external_id: ExternalSessionId::new("s1"),
                },
                terminal_id: TerminalId::new(terminal_id),
            },
            log_epoch: LogEpoch::new("018f0e2c-0f5a-7c3a-9b1e-0a1b2c3d4e5f"),
        }
    }

    fn pos(line: u64, byte_offset: u64) -> HistoryPosition {
        HistoryPosition::new(line, byte_offset).expect("valid test position")
    }

    fn at(line: u64, byte_offset: u64) -> Option<ReadStart> {
        Some(ReadStart::At(pos(line, byte_offset)))
    }

    #[test]
    fn read_limits_default_and_hard_cap_are_pinned() {
        assert_eq!(ReadLimits::DEFAULT.max_lines(), 200);
        assert_eq!(ReadLimits::DEFAULT.max_bytes(), 32 * 1024);
        assert_eq!(ReadLimits::HARD_CAP.max_lines(), 1000);
        assert_eq!(ReadLimits::HARD_CAP.max_bytes(), 256 * 1024);
        assert_eq!(ReadLimits::default(), ReadLimits::DEFAULT);

        // Zero budgets can never make progress.
        assert_eq!(
            ReadLimits::new(0, 4096),
            Err(LimitError::Zero { field: "max_lines" })
        );
        assert_eq!(
            ReadLimits::new(10, 0),
            Err(LimitError::Zero { field: "max_bytes" })
        );

        // Above the hard cap is clamped, never honored.
        let huge = ReadLimits::new(u32::MAX, u32::MAX)
            .expect("non-zero limits are accepted")
            .clamped();
        assert_eq!(huge, ReadLimits::HARD_CAP);

        // Within the caps nothing changes.
        let modest = ReadLimits::new(10, 1024).expect("valid").clamped();
        assert_eq!(modest.max_lines(), 10);
        assert_eq!(modest.max_bytes(), 1024);
    }

    #[test]
    fn read_request_binds_one_log_and_one_position_source() {
        let cursor = ReadCursor::new(log("t1"), pos(3, 0), 10).expect("3 <= 10");

        // A continuation cursor of the same log is accepted.
        assert!(
            ReadRequest::new(log("t1"), None, Some(cursor.clone()), ReadLimits::DEFAULT).is_ok()
        );

        // A cursor of another epoch/terminal is refused, never re-anchored.
        let other_epoch = LogIdentity {
            log_epoch: LogEpoch::new("018f0e2c-0f5a-7c3a-9b1e-0a1b2c3d4e60"),
            ..log("t1")
        };
        assert!(matches!(
            ReadRequest::new(other_epoch, None, Some(cursor.clone()), ReadLimits::DEFAULT),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            ReadRequest::new(log("t2"), None, Some(cursor.clone()), ReadLimits::DEFAULT),
            Err(QueryError::Invalid { .. })
        ));

        // Start and cursor together are contradictory.
        assert!(matches!(
            ReadRequest::new(log("t1"), at(1, 0), Some(cursor), ReadLimits::DEFAULT),
            Err(QueryError::Invalid { .. })
        ));

        // A first-page read may omit the start (earliest readable), and the
        // three start intentions stay distinguishable.
        let first = ReadRequest::first(log("t1"), None, ReadLimits::DEFAULT).expect("valid");
        assert_eq!(first.start(), ReadStart::Earliest);
        assert_eq!(first.limits(), ReadLimits::DEFAULT);
        let newest = ReadRequest::first(log("t1"), Some(ReadStart::Newest), ReadLimits::DEFAULT)
            .expect("valid");
        assert_eq!(newest.start(), ReadStart::Newest);
        let explicit = ReadRequest::first(log("t1"), at(4, 0), ReadLimits::DEFAULT).expect("valid");
        assert_eq!(explicit.start(), ReadStart::At(pos(4, 0)));

        // Requests always carry clamped limits.
        let clamped = ReadRequest::first(
            log("t1"),
            at(1, 0),
            ReadLimits::new(u32::MAX, u32::MAX).expect("non-zero"),
        )
        .expect("valid");
        assert_eq!(clamped.limits(), ReadLimits::HARD_CAP);
    }

    #[test]
    fn a_complete_page_cannot_carry_a_continuation() {
        let cursor = ReadCursor::new(log("t1"), pos(4, 0), 10).expect("4 <= 10");
        let fragment = LineFragment::new(pos(3, 0), "line", false, false);
        let retained = HistoryRange::new(pos(3, 0), pos(10, 0)).expect("ordered");

        // No truncation plus a continuation is the contradiction.
        assert!(matches!(
            ReadPage::new(
                vec![fragment.clone()],
                Some(cursor.clone()),
                None,
                Some(retained),
                false
            ),
            Err(QueryError::Invalid { .. })
        ));

        // No truncation and no continuation: complete.
        let complete = ReadPage::new(vec![fragment.clone()], None, None, Some(retained), false)
            .expect("complete page");
        assert!(complete.next().is_none());
        assert_eq!(complete.truncation(), None);
        assert_eq!(complete.fragments().len(), 1);
        assert!(!complete.degraded());

        // Truncated with a continuation: a resumable partial page.
        let partial = ReadPage::new(
            vec![fragment],
            Some(cursor),
            Some(ReadTruncation::LineBudget),
            Some(retained),
            true,
        )
        .expect("partial page");
        assert_eq!(partial.truncation(), Some(ReadTruncation::LineBudget));
        assert!(partial.next().is_some());
        assert!(partial.degraded());

        // A page that ends at a gap has a reason but no continuation: the
        // caller must ask explicitly for the next readable range.
        let gap = ReadPage::new(
            Vec::new(),
            None,
            Some(ReadTruncation::Gap),
            Some(retained),
            true,
        )
        .expect("gap page");
        assert_eq!(gap.truncation(), Some(ReadTruncation::Gap));
        assert!(gap.next().is_none());
    }

    #[test]
    fn fragment_keeps_its_in_line_offset_and_omission_flags() {
        let fragment = LineFragment::new(pos(9, 4096), "tail", true, true);
        assert_eq!(fragment.position(), pos(9, 4096));
        assert_eq!(fragment.text(), "tail");
        assert!(fragment.prefix_omitted());
        assert!(fragment.suffix_remaining());

        // An empty line fragment is legal history: a line can be empty.
        let empty = LineFragment::new(pos(2, 0), "", false, false);
        assert_eq!(empty.text(), "");
    }

    #[test]
    fn read_result_keeps_the_fixed_cursor_with_the_page() {
        let cursor = ReadCursor::new(log("t1"), pos(40, 0), 42).expect("40 <= 42");
        let page = ReadPage::new(Vec::new(), None, None, None, false).expect("complete");
        let result = ReadResult::new(cursor.clone(), page);
        assert_eq!(result.cursor(), &cursor);
        assert_eq!(result.cursor().end_line(), 42);
        assert!(result.page().fragments().is_empty());
    }

    #[test]
    fn tail_snapshot_marks_an_omitted_prefix() {
        let snapshot = TailSnapshot::new(
            log("t1"),
            TailPosition::new(TailId::new("tail-1"), 7, 4096),
            "suffix",
            true,
        );
        assert_eq!(snapshot.position().byte_offset(), 4096);
        assert_eq!(snapshot.position().revision(), 7);
        assert!(snapshot.truncated());
        assert_eq!(snapshot.text(), "suffix");

        // An untruncated empty tail is representable (nothing written yet).
        let empty = TailSnapshot::new(
            log("t1"),
            TailPosition::new(TailId::new("tail-1"), 0, 0),
            "",
            false,
        );
        assert!(!empty.truncated());
        assert!(empty.text().is_empty());
    }

    #[test]
    fn grep_query_rejects_malformed_ranges_and_needles() {
        let identity = log("t1");
        assert!(matches!(
            GrepQuery::new(identity.clone(), "", true, pos(1, 0), 5, 0),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            GrepQuery::new(identity.clone(), "x", true, pos(6, 0), 5, 0),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            GrepQuery::new(
                identity.clone(),
                "x".repeat(GrepQuery::MAX_NEEDLE_BYTES + 1),
                true,
                pos(1, 0),
                5,
                0
            ),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            GrepQuery::new(
                identity.clone(),
                "x",
                true,
                pos(1, 0),
                5,
                GrepQuery::MAX_CONTEXT_LINES + 1
            ),
            Err(QueryError::Invalid { .. })
        ));

        let query =
            GrepQuery::new(identity, "needle", false, pos(2, 0), 9, 2).expect("valid query");
        assert_eq!(query.needle(), "needle");
        assert!(!query.case_sensitive());
        assert_eq!(query.start(), pos(2, 0));
        assert_eq!(query.end_line(), 9);
        assert_eq!(query.context_lines(), 2);
    }

    #[test]
    fn grep_limits_keep_scan_and_response_budgets_separate() {
        assert_eq!(GrepLimits::DEFAULT.max_matches(), 200);
        assert_eq!(GrepLimits::DEFAULT.max_bytes(), 32 * 1024);
        assert_eq!(GrepLimits::DEFAULT.scan_bytes(), 4 * 1024 * 1024);
        assert_eq!(GrepLimits::HARD_CAP.max_matches(), 1000);
        assert_eq!(GrepLimits::HARD_CAP.max_bytes(), 256 * 1024);
        assert_eq!(GrepLimits::HARD_CAP.scan_bytes(), 64 * 1024 * 1024);
        assert_eq!(GrepLimits::default(), GrepLimits::DEFAULT);

        assert_eq!(
            GrepLimits::new(0, 1024, 1024),
            Err(LimitError::Zero {
                field: "max_matches"
            })
        );
        assert_eq!(
            GrepLimits::new(1, 0, 1024),
            Err(LimitError::Zero { field: "max_bytes" })
        );
        assert_eq!(
            GrepLimits::new(1, 1024, 0),
            Err(LimitError::Zero {
                field: "scan_bytes"
            })
        );

        let clamped = GrepLimits::new(u32::MAX, u32::MAX, u64::MAX)
            .expect("non-zero")
            .clamped();
        assert_eq!(clamped, GrepLimits::HARD_CAP);
    }

    #[test]
    fn grep_scan_point_matches_only_its_exact_query() {
        let identity = log("t1");
        let query =
            GrepQuery::new(identity.clone(), "needle", true, pos(2, 0), 9, 1).expect("valid query");
        let point = GrepScanPoint::new(query.clone(), pos(4, 128)).expect("inside the range");
        assert!(point.matches(&query));
        assert_eq!(point.next(), pos(4, 128));

        // Changing any bound query parameter invalidates the point.
        let changed_needle =
            GrepQuery::new(identity.clone(), "other", true, pos(2, 0), 9, 1).expect("valid");
        let changed_case =
            GrepQuery::new(identity.clone(), "needle", false, pos(2, 0), 9, 1).expect("valid");
        let changed_context =
            GrepQuery::new(identity.clone(), "needle", true, pos(2, 0), 9, 2).expect("valid");
        let changed_range =
            GrepQuery::new(identity.clone(), "needle", true, pos(3, 0), 9, 1).expect("valid");
        let changed_end =
            GrepQuery::new(identity.clone(), "needle", true, pos(2, 0), 10, 1).expect("valid");
        let changed_log = GrepQuery::new(
            {
                let mut other = identity.clone();
                other.terminal.terminal_id = TerminalId::new("t2");
                other
            },
            "needle",
            true,
            pos(2, 0),
            9,
            1,
        )
        .expect("valid");

        for changed in [
            changed_needle,
            changed_case,
            changed_context,
            changed_range,
            changed_end,
            changed_log,
        ] {
            assert!(!point.matches(&changed));
            assert!(matches!(
                GrepRequest::new(changed, Some(point.clone())),
                Err(QueryError::Invalid { .. })
            ));
        }

        // The exact query resumes.
        assert!(GrepRequest::new(query.clone(), Some(point.clone())).is_ok());
        assert!(GrepRequest::fresh(query.clone()).resume().is_none());
        assert_eq!(GrepRequest::fresh(query.clone()).query().needle(), "needle");

        // A point outside its own range is impossible to construct.
        assert!(matches!(
            GrepScanPoint::new(query.clone(), pos(10, 0)),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            GrepScanPoint::new(query, pos(1, 0)),
            Err(QueryError::Invalid { .. })
        ));
    }

    #[test]
    fn a_partial_grep_page_cannot_claim_completeness() {
        let identity = log("t1");
        let query = GrepQuery::new(identity, "x", true, pos(1, 0), 5, 0).expect("valid");
        let scanned = HistoryRange::new(pos(1, 0), pos(3, 0)).expect("ordered");
        let point = GrepScanPoint::new(query.clone(), pos(3, 0)).expect("inside range");
        let match_ = GrepMatch::new(pos(1, 2), 1).expect("non-zero length");

        // A complete scan carries no continuation.
        let complete = GrepPage::new(
            vec![match_],
            Vec::new(),
            scanned,
            None,
            GrepStop::RangeExhausted,
            false,
            false,
        )
        .expect("complete page");
        assert!(complete.next_scan().is_none());
        assert_eq!(complete.stopped(), GrepStop::RangeExhausted);

        // Zero matches with a cut-short scan is not "no match in range".
        let partial = GrepPage::new(
            Vec::new(),
            Vec::new(),
            scanned,
            Some(point.clone()),
            GrepStop::ScanBudget,
            false,
            false,
        )
        .expect("partial page");
        assert!(partial.matches().is_empty());
        assert_eq!(partial.stopped(), GrepStop::ScanBudget);
        assert_eq!(partial.next_scan(), Some(&point));
        assert!(!partial.degraded());

        // Neither contradiction is constructible.
        assert!(matches!(
            GrepPage::new(
                Vec::new(),
                Vec::new(),
                scanned,
                Some(point.clone()),
                GrepStop::RangeExhausted,
                false,
                false
            ),
            Err(QueryError::Invalid { .. })
        ));
        assert!(matches!(
            GrepPage::new(
                Vec::new(),
                Vec::new(),
                scanned,
                None,
                GrepStop::ResponseBudget,
                false,
                false
            ),
            Err(QueryError::Invalid { .. })
        ));
    }

    #[test]
    fn grep_match_and_context_constructors_check_their_invariants() {
        assert!(matches!(
            GrepMatch::new(pos(1, 0), 0),
            Err(QueryError::Invalid { .. })
        ));
        assert_eq!(
            GrepMatch::new(pos(1, 4), 3).expect("non-zero length"),
            GrepMatch {
                position: pos(1, 4),
                length: 3
            }
        );

        assert!(matches!(
            GrepContext::new(0, "x", false),
            Err(QueryError::Invalid { .. })
        ));
        let context = GrepContext::new(7, "text", true).expect("valid context line");
        assert_eq!(context.line(), 7);
        assert_eq!(context.text(), "text");
        assert!(context.truncated());
    }

    #[test]
    fn query_error_distinguishes_expiry_from_a_malformed_request() {
        let expired = QueryError::CursorExpired {
            earliest: Some(pos(9, 0)),
            missing: Some(HistoryRange::new(pos(3, 0), pos(9, 0)).expect("ordered")),
        };
        assert!(expired.is_expired());
        assert!(
            QueryError::Gap {
                range: HistoryRange::new(pos(3, 0), pos(4, 0)).expect("ordered"),
            }
            .is_expired()
        );
        assert!(
            !QueryError::Invalid {
                detail: "bad request".into()
            }
            .is_expired()
        );
    }

    #[test]
    fn tail_view_pairs_history_with_the_tail() {
        let cursor = ReadCursor::new(log("t1"), pos(4, 0), 4).expect("4 <= 4");
        let page = ReadPage::new(Vec::new(), None, None, None, false).expect("complete");
        let history = ReadResult::new(cursor, page);
        let tail = TailSnapshot::new(
            log("t1"),
            TailPosition::new(TailId::new("tail-9"), 2, 0),
            "partial",
            false,
        );
        let view = TailView::new(history, tail);
        assert_eq!(view.history().cursor().end_line(), 4);
        assert_eq!(view.tail().text(), "partial");
    }
}
