//! Production query surface: fixed-range normalized reads and literal
//! grep over committed history (S4).
//!
//! Both queries scan the **frames** of the committed normalized stream
//! directly. Concatenating a stream first (the S2 verification read) would
//! materialize the whole history, would lose each frame's line, in-line
//! offset, and last-of-line flag, and could therefore neither page an
//! over-long line by `byte_offset` nor enforce a fixed `end_line` — all of
//! which this surface must do. One segment is read and validated at a time
//! (a segment is at most its rotation bound), so one page's allocation is
//! bounded by an explicit budget rather than by the retained history.
//!
//! Guarantees this module owns:
//!
//! - A read's `end_line` is minted with its first page (at the committed
//!   `line_watermark`) and never grows: later appends are invisible to an
//!   already minted page.
//! - A page never splits a UTF-8 character: frame payloads are already cut
//!   on character boundaries, and a page cut inside a payload walks
//!   forward to the next boundary, so every returned fragment and every
//!   returned `byte_offset` is a rune boundary.
//! - Retention, epoch, gap, and query mismatches are typed refusals
//!   ([`QueryError::CursorExpired`] / [`QueryError::Gap`] /
//!   [`QueryError::Invalid`]) carrying the earliest readable position and
//!   the missing range; a missing range is never silently skipped or
//!   stitched across.
//! - The terminal's `degraded` latch (an unrecoverable or unlocated loss)
//!   is reported on every page, so a served page is never mistaken for
//!   proof that no output was lost.
//! - Grep keeps its expected-work budget (`scan_bytes`) separate from its
//!   response budget (`max_matches` / `max_bytes`); a page that stopped
//!   early always carries the point to resume at and never reports
//!   completeness — including when it found no match at all.
//! - A literal is matched across frame boundaries and inside arbitrarily
//!   long lines with bounded matcher state. A page always scans whole
//!   lines, and a resumed page preloads the bytes before its resume offset,
//!   so a match is found exactly once no matter where a frame, a page, or a
//!   budget boundary falls.

use std::collections::VecDeque;

use qingluan_core::terminal::{
    GrepContext, GrepLimits, GrepMatch, GrepPage, GrepRequest, GrepScanPoint, GrepStop,
    HistoryPosition, HistoryRange, LineFragment, LogEpoch, LogIdentity, QueryError, ReadCursor,
    ReadPage, ReadRequest, ReadResult, ReadStart, ReadTruncation, TerminalRef,
};

use crate::db::{SegmentRow, TerminalRow};
use crate::error::StorageError;
use crate::frame::ScannedFrame;
use crate::gap::GapSpan;
use crate::identity::{HeaderIdentity, LogKey, ResolvedIdentity};
use crate::recovery::{ChainLink, validate_chain};
use crate::scan::scan_committed;
use crate::{LogStore, LogStream};

/// Response bound on one context line; a longer line is shortened and
/// flagged, never silently cut.
const CONTEXT_LINE_MAX_BYTES: usize = 512;

/// Charged against the response budget per returned match.
const MATCH_RESPONSE_BYTES: usize = 48;

fn expired(earliest: Option<HistoryPosition>, missing: Option<HistoryRange>) -> StorageError {
    StorageError::Query(QueryError::CursorExpired { earliest, missing })
}

fn invalid(detail: impl Into<String>) -> StorageError {
    StorageError::Query(QueryError::Invalid {
        detail: detail.into(),
    })
}

fn recovery(detail: impl Into<String>) -> StorageError {
    StorageError::RecoveryRequired {
        detail: detail.into(),
    }
}

fn pos(line: u64, byte_offset: u64) -> Result<HistoryPosition, StorageError> {
    HistoryPosition::new(line, byte_offset).map_err(|error| invalid(error.to_string()))
}

/// The later of two positions (by line, then in-line offset).
fn max_position(a: HistoryPosition, b: HistoryPosition) -> HistoryPosition {
    if (a.line(), a.byte_offset()) >= (b.line(), b.byte_offset()) {
        a
    } else {
        b
    }
}

/// One validated sample of a terminal's committed normalized history.
struct Committed {
    identity: HeaderIdentity,
    terminal: TerminalRow,
    /// Explicit gaps of the normalized stream, oldest first.
    gaps: Vec<GapSpan>,
    /// Surviving (active or sealed, non-empty) normalized segments, oldest
    /// first.
    segments: Vec<SegmentRow>,
}

impl Committed {
    /// Earliest position still meaningful for this log: the retained
    /// floor, or the next line to be written when everything committed was
    /// reclaimed.
    fn earliest(&self) -> Result<HistoryPosition, StorageError> {
        let watermark = self.terminal.line_watermark;
        if watermark == 0 {
            return pos(1, 0);
        }
        let floor = self.terminal.retained_first_line.max(1);
        if floor > watermark {
            return match watermark.checked_add(1) {
                Some(next) => pos(next, 0),
                None => pos(watermark, 0),
            };
        }
        pos(floor, 0)
    }

    /// The currently retained (readable) span, absent when nothing is
    /// committed or nothing committed is still backed by a segment.
    fn retained(&self) -> Result<Option<HistoryRange>, StorageError> {
        if self.terminal.line_watermark == 0 || self.segments.is_empty() {
            return Ok(None);
        }
        let floor = self.terminal.retained_first_line.max(1);
        if floor > self.terminal.line_watermark {
            return Ok(None);
        }
        HistoryRange::new(pos(floor, 0)?, pos(self.terminal.line_watermark, 0)?)
            .map(Some)
            .map_err(|error| invalid(error.to_string()))
    }

    /// The first explicit gap that starts after `line` and at or before
    /// `end_line`. A gap covering `line` itself is a refusal, never a
    /// stop-and-continue.
    fn gap_after(&self, line: u64, end_line: u64) -> Option<GapSpan> {
        self.gaps
            .iter()
            .filter(|gap| gap.start > line && gap.start <= end_line)
            .min_by_key(|gap| gap.start)
            .copied()
    }

    /// The explicit gap covering `line`, if any.
    fn gap_covering(&self, line: u64) -> Option<GapSpan> {
        self.gaps
            .iter()
            .find(|gap| gap.start <= line && line < gap.end)
            .copied()
    }

    /// Refuse a position that is no longer readable — below the retained
    /// floor or inside an explicitly recorded gap — carrying the earliest
    /// readable position and the missing range. The position is never
    /// silently re-anchored.
    fn require_readable(&self, at: HistoryPosition) -> Result<(), StorageError> {
        let earliest = self.earliest()?;
        if at.line() < earliest.line() {
            let missing =
                HistoryRange::new(at, earliest).map_err(|error| invalid(error.to_string()))?;
            return Err(expired(Some(earliest), Some(missing)));
        }
        if let Some(gap) = self.gap_covering(at.line()) {
            let after = pos(gap.end, 0)?;
            let missing = HistoryRange::new(pos(gap.start, 0)?, after)
                .map_err(|error| invalid(error.to_string()))?;
            return Err(expired(Some(after), Some(missing)));
        }
        Ok(())
    }
}

/// The next UTF-8 character boundary at or after `index`.
fn ceil_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// The largest UTF-8 character boundary at or below `len`.
fn floor_boundary(text: &str, len: usize) -> usize {
    let mut len = len.min(text.len());
    while len > 0 && !text.is_char_boundary(len) {
        len -= 1;
    }
    len
}

fn payload_str<'a>(frame: &ScannedFrame, data: &'a [u8]) -> Result<&'a str, StorageError> {
    let payload = &data[frame.payload_at..frame.end - 4];
    std::str::from_utf8(payload).map_err(|_| {
        recovery(format!(
            "normalized segment payload at byte {} is not UTF-8; run recovery before reading",
            frame.payload_at
        ))
    })
}

/// The last `keep` rune-aligned bytes of the line that precede `before`:
/// the preload a resuming page needs so a match straddling its resume
/// offset is still found. Bounded by `keep` plus one character, whatever
/// the line's length.
fn preceding_text(
    frames: &[&ScannedFrame],
    data: &[u8],
    before: u64,
    keep: usize,
) -> Result<String, StorageError> {
    let mut buffer = String::new();
    for frame in frames {
        let payload = payload_str(frame, data)?;
        let chunk_start = frame.header.line_offset;
        if chunk_start >= before {
            break;
        }
        let end = floor_boundary(
            payload,
            ((before - chunk_start) as usize).min(payload.len()),
        );
        if end == 0 {
            continue;
        }
        buffer.push_str(&payload[..end]);
        if buffer.len() > keep {
            let drop = ceil_boundary(&buffer, buffer.len() - keep);
            buffer.drain(..drop);
        }
    }
    Ok(buffer)
}

/// One line's text sliced out of the frames that carry it.
struct SlicedLine {
    text: String,
    /// Absolute in-line offset of `text`'s first byte (rune-aligned).
    start_offset: u64,
    /// Whether the byte budget cut the line short.
    cut: bool,
    /// Absolute in-line offset where the next page resumes (only
    /// meaningful when `cut`).
    resume_offset: u64,
}

/// Slice one line's payload out of `frames` (all carrying the same line, in
/// order) starting at `start_offset` and taking at most `byte_budget`
/// bytes.
///
/// `start_offset` is clamped forward to the line's length and to a UTF-8
/// character boundary, so the returned text and the returned offsets are
/// always rune-aligned even when a caller offers an offset inside a
/// character.
fn slice_line(
    frames: &[&ScannedFrame],
    data: &[u8],
    start_offset: u64,
    byte_budget: usize,
) -> Result<SlicedLine, StorageError> {
    let mut text = String::new();
    let mut actual_start: Option<u64> = None;
    let mut line_len = 0u64;
    for frame in frames {
        let payload = payload_str(frame, data)?;
        let chunk_start = frame.header.line_offset;
        let chunk_len = payload.len() as u64;
        line_len = chunk_start + chunk_len;
        let from = start_offset.max(chunk_start);
        if from >= line_len {
            continue;
        }
        let local = ceil_boundary(payload, (from - chunk_start) as usize);
        if actual_start.is_none() {
            actual_start = Some(chunk_start + local as u64);
        }
        let available = &payload[local..];
        let room = byte_budget.saturating_sub(text.len());
        if available.len() <= room {
            text.push_str(available);
            continue;
        }
        let take = floor_boundary(available, room);
        text.push_str(&available[..take]);
        let resume_offset = chunk_start + local as u64 + take as u64;
        return Ok(SlicedLine {
            text,
            start_offset: actual_start.expect("a slice was started"),
            cut: true,
            resume_offset,
        });
    }
    Ok(SlicedLine {
        text,
        start_offset: actual_start.unwrap_or(start_offset.min(line_len)),
        cut: false,
        resume_offset: 0,
    })
}

/// Streaming literal matcher with bounded state.
///
/// The needle plus one rolling window of recently folded text is all the
/// state there is, so an arbitrarily long line is matched across frame and
/// page boundaries without ever being materialized. Case-insensitive mode
/// folds with `char::to_lowercase` per character — an approximation of the
/// Unicode case rules, applied identically to the needle and to the text,
/// that only ever reports matches whose start and end coincide with
/// character boundaries.
struct LiteralMatcher {
    needle: Vec<u8>,
    case_sensitive: bool,
    window: Vec<u8>,
    /// Absolute folded position of `window[0]`.
    window_start: u64,
    /// Absolute folded position just past the window.
    window_end: u64,
    /// Folded and original span of every character in the window.
    chars: VecDeque<CharSpan>,
    /// Original in-line offset below which a match start is not reported
    /// (already returned by the page this one resumes).
    report_from: u64,
    /// Whether matches are reported at all (preload suppresses them).
    reporting: bool,
    /// The match found by the most recent character, if any.
    found: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy)]
struct CharSpan {
    folded_start: u64,
    folded_end: u64,
    orig_start: u64,
    orig_end: u64,
}

impl LiteralMatcher {
    fn new(needle: &str, case_sensitive: bool) -> Self {
        let folded = if case_sensitive {
            needle.as_bytes().to_vec()
        } else {
            needle.to_lowercase().into_bytes()
        };
        Self {
            needle: folded,
            case_sensitive,
            window: Vec::new(),
            window_start: 0,
            window_end: 0,
            chars: VecDeque::new(),
            report_from: 0,
            reporting: true,
            found: None,
        }
    }

    fn set_report_from(&mut self, offset: u64) {
        self.report_from = offset;
    }

    /// Bytes of the preload a resuming page needs before its first byte.
    fn preload_bytes(&self) -> usize {
        self.needle.len().max(1)
    }

    /// Feed text whose first byte sits at `orig_offset` inside the line,
    /// returning the (start, end) original offsets of the matches that end
    /// inside it. Matches never overlap: each match resets the window.
    fn feed(&mut self, text: &str, orig_offset: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut offset = orig_offset;
        for ch in text.chars() {
            self.push(ch, offset);
            offset += ch.len_utf8() as u64;
            if let Some(found) = self.found.take() {
                out.push(found);
                self.reset_after_match();
            }
        }
        out
    }

    /// Feed text that precedes the page's first reported byte: it completes
    /// the window so a match straddling the resume offset is still found,
    /// but reports nothing itself.
    fn preload(&mut self, text: &str, orig_offset: u64) {
        let reporting = self.reporting;
        self.reporting = false;
        let mut offset = orig_offset;
        for ch in text.chars() {
            self.push(ch, offset);
            offset += ch.len_utf8() as u64;
            self.found = None;
        }
        self.reporting = reporting;
    }

    fn push(&mut self, ch: char, orig_offset: u64) {
        let folded: Vec<u8> = if self.case_sensitive {
            let mut buffer = [0u8; 4];
            ch.encode_utf8(&mut buffer).as_bytes().to_vec()
        } else {
            ch.to_lowercase().collect::<String>().into_bytes()
        };
        let folded_len = folded.len() as u64;
        self.chars.push_back(CharSpan {
            folded_start: self.window_end,
            folded_end: self.window_end + folded_len,
            orig_start: orig_offset,
            orig_end: orig_offset + ch.len_utf8() as u64,
        });
        self.window.extend_from_slice(&folded);
        self.window_end += folded_len;
        if self.reporting && self.window.len() >= self.needle.len() {
            let at = self.window.len() - self.needle.len();
            if self.window[at..] == self.needle[..] {
                let folded_start = self.window_start + at as u64;
                let folded_end = folded_start + self.needle.len() as u64;
                if let Some((start, end)) = self.resolve(folded_start, folded_end)
                    && start >= self.report_from
                {
                    self.found = Some((start, end));
                }
            }
        }
        self.trim();
    }

    /// Whether the two folded positions coincide with character boundaries
    /// of the window (a case fold can expand one character into several, so
    /// an offset inside an expansion is not a valid text position).
    fn resolve(&self, folded_start: u64, folded_end: u64) -> Option<(u64, u64)> {
        let start = self
            .chars
            .iter()
            .find(|span| span.folded_start == folded_start)?;
        let end = self
            .chars
            .iter()
            .find(|span| span.folded_end == folded_end)?;
        Some((start.orig_start, end.orig_end))
    }

    /// Drop whole characters from the front while at least one needle's
    /// worth of folded bytes is still kept: the state stays bounded by the
    /// needle plus one character, whatever the line's length.
    fn trim(&mut self) {
        while self.chars.len() > 1 {
            let Some(front) = self.chars.front().copied() else {
                break;
            };
            let drop = (front.folded_end - front.folded_start) as usize;
            if self.window.len().saturating_sub(drop) < self.needle.len() {
                break;
            }
            self.window.drain(..drop);
            self.window_start += drop as u64;
            self.chars.pop_front();
        }
    }

    fn reset_after_match(&mut self) {
        self.window.clear();
        self.chars.clear();
        self.window_start = self.window_end;
    }
}

impl LogStore {
    /// The persisted log identity (epoch) of one terminal, for minting
    /// cursors. Works with or without a live handle: the terminal row is
    /// the authority.
    pub async fn log_identity(&self, terminal: &TerminalRef) -> Result<LogIdentity, StorageError> {
        let key = LogKey::of(terminal);
        let row =
            self.store.terminal(&key).await?.ok_or_else(|| {
                StorageError::UnknownLog(terminal.terminal_id.as_str().to_owned())
            })?;
        Ok(LogIdentity {
            terminal: terminal.clone(),
            log_epoch: LogEpoch::new(uuid::Uuid::from_bytes(row.log_epoch).to_string()),
        })
    }

    /// Serve one page of a fixed-range read of committed normalized lines.
    ///
    /// The first page mints its fixed `end_line` at the committed
    /// watermark and never extends it; a page served with a cursor keeps
    /// that cursor's bound, so later appends stay invisible. A requested
    /// start past the committed watermark is a benign empty page (not a
    /// refusal). A start below the retained floor or inside an explicit
    /// gap is [`QueryError::CursorExpired`]; a range interrupted by a gap
    /// ends the page with [`ReadTruncation::Gap`] and no continuation, so
    /// continuing past the hole stays the caller's explicit choice.
    pub async fn read(&self, request: &ReadRequest) -> Result<ReadResult, StorageError> {
        let log = request.log();
        let committed = self.committed_normalized(log).await?;
        let limits = request.limits();
        let earliest = committed.earliest()?;
        let retained = committed.retained()?;
        let watermark = committed.terminal.line_watermark;
        let degraded = committed.terminal.degraded;

        let (start, end_line) = match request.cursor() {
            Some(cursor) => (cursor.next(), cursor.end_line()),
            None => {
                let start = match request.start() {
                    ReadStart::At(position) => position,
                    // The newest lines that fit the line budget. The window
                    // is this server's choice, so it is clamped forward past
                    // the retained floor and past any explicit gap instead
                    // of refusing; `degraded` still reports the loss, and a
                    // caller-chosen position is never clamped.
                    ReadStart::Newest if watermark > 0 => {
                        let lines = u64::from(limits.max_lines()).max(1);
                        let candidate = watermark.saturating_sub(lines - 1).max(earliest.line());
                        let mut window = pos(candidate, 0)?;
                        while let Some(gap) = committed.gap_covering(window.line()) {
                            window = pos(gap.end, 0)?;
                        }
                        window
                    }
                    ReadStart::Newest | ReadStart::Earliest => earliest,
                };
                // A start past the committed watermark is bounded by the
                // requested line itself, so nothing already committed is
                // excluded by the fixed bound.
                (start, watermark.max(start.line()))
            }
        };
        let cursor = ReadCursor::new(log.clone(), start, end_line)
            .map_err(|error| invalid(error.to_string()))?;

        committed.require_readable(start)?;
        if start.line() > watermark {
            let page = ReadPage::new(Vec::new(), None, None, retained, degraded)
                .map_err(|error| invalid(error.to_string()))?;
            return Ok(ReadResult::new(cursor, page));
        }

        let gap = committed.gap_after(start.line(), end_line);
        let read_end = gap.map_or(end_line, |gap| gap.start.saturating_sub(1));
        let stopped_at_gap = read_end < end_line;

        let byte_budget = limits.max_bytes() as usize;
        let mut fragments: Vec<LineFragment> = Vec::new();
        let mut bytes_used: usize = 0;
        let mut lines_used: u32 = 0;
        let mut next: Option<ReadCursor> = None;
        let mut truncation: Option<ReadTruncation> = None;

        'segments: for segment in &committed.segments {
            let Some((seg_first, seg_last)) = segment.stream_range() else {
                continue;
            };
            if seg_last <= start.line() || seg_first > read_end {
                continue;
            }
            let scan = scan_committed(segment, &self.root, &committed.identity).await?;
            let mut index = 0usize;
            while index < scan.report.frames.len() {
                let line = scan.report.frames[index].header.line;
                let group_end = scan.report.frames[index..]
                    .iter()
                    .position(|frame| frame.header.line != line)
                    .map_or(scan.report.frames.len(), |offset| index + offset);
                let group: Vec<&ScannedFrame> =
                    scan.report.frames[index..group_end].iter().collect();
                index = group_end;
                if line < start.line() {
                    continue;
                }
                if line > read_end {
                    break 'segments;
                }
                if bytes_used >= byte_budget || lines_used >= limits.max_lines() {
                    // The budget is enforced between whole lines, so a
                    // returned fragment is never a cut the caller cannot
                    // continue from.
                    truncation = Some(if bytes_used >= byte_budget {
                        ReadTruncation::ByteBudget
                    } else {
                        ReadTruncation::LineBudget
                    });
                    next = Some(
                        ReadCursor::new(log.clone(), pos(line, 0)?, end_line)
                            .map_err(|error| invalid(error.to_string()))?,
                    );
                    break 'segments;
                }
                let line_start = if line == start.line() {
                    let floor = if line == earliest.line() {
                        earliest.byte_offset()
                    } else {
                        0
                    };
                    start.byte_offset().max(floor)
                } else {
                    0
                };
                let sliced = slice_line(&group, &scan.data, line_start, byte_budget - bytes_used)?;
                bytes_used += sliced.text.len();
                let fragment = LineFragment::new(
                    pos(line, sliced.start_offset)?,
                    sliced.text,
                    sliced.start_offset > 0,
                    sliced.cut,
                );
                fragments.push(fragment);
                if sliced.cut {
                    truncation = Some(ReadTruncation::ByteBudget);
                    next = Some(
                        ReadCursor::new(log.clone(), pos(line, sliced.resume_offset)?, end_line)
                            .map_err(|error| invalid(error.to_string()))?,
                    );
                    break 'segments;
                }
                lines_used += 1;
            }
        }

        if truncation.is_none() && stopped_at_gap {
            // An explicit hole inside the fixed range: the page stops
            // before it and offers no continuation, so continuing past the
            // loss is the caller's explicit choice and is never stitched.
            truncation = Some(ReadTruncation::Gap);
        }
        let page = ReadPage::new(fragments, next, truncation, retained, degraded)
            .map_err(|error| invalid(error.to_string()))?;
        Ok(ReadResult::new(cursor, page))
    }

    /// Serve one page of a literal grep over the committed part of a fixed
    /// history range.
    ///
    /// The phrase is matched literally (optionally case-insensitively, with
    /// `char::to_lowercase` folding applied identically to the needle and
    /// the text) and always across whole lines, so a match is never missed
    /// because of where a frame, a page, or a budget boundary fell. The
    /// scanned-byte budget is checked between lines; the response budget
    /// (matches and bytes) stops the page with the point to resume at. A
    /// range that intersects an explicit gap is refused as
    /// [`QueryError::Gap`], so the scan can never claim to have covered a
    /// hole.
    pub async fn grep(
        &self,
        request: &GrepRequest,
        limits: GrepLimits,
    ) -> Result<GrepPage, StorageError> {
        let limits = limits.clamped();
        let query = request.query();
        let committed = self.committed_normalized(query.log()).await?;
        let degraded = committed.terminal.degraded;
        let watermark = committed.terminal.line_watermark;
        let start = request
            .resume()
            .map_or_else(|| query.start(), GrepScanPoint::next);

        committed.require_readable(start)?;
        // Only committed lines can be scanned; a fixed range above the
        // watermark simply has nothing yet.
        let scan_end = query.end_line().min(watermark);
        if start.line() > scan_end {
            let scanned =
                HistoryRange::new(start, start).map_err(|error| invalid(error.to_string()))?;
            let page = GrepPage::new(
                Vec::new(),
                Vec::new(),
                scanned,
                None,
                GrepStop::RangeExhausted,
                false,
                degraded,
            )
            .map_err(|error| invalid(error.to_string()))?;
            return Ok(page);
        }
        if let Some(gap) = committed.gap_after(start.line(), scan_end) {
            let range = HistoryRange::new(pos(gap.start, 0)?, pos(gap.end, 0)?)
                .map_err(|error| invalid(error.to_string()))?;
            return Err(StorageError::Query(QueryError::Gap { range }));
        }

        let context_lines = query.context_lines();
        let mut matcher = LiteralMatcher::new(query.needle(), query.case_sensitive());
        let mut matches: Vec<GrepMatch> = Vec::new();
        let mut contexts: Vec<GrepContext> = Vec::new();
        let mut contexts_truncated = false;
        let mut ring: VecDeque<(u64, String, bool)> = VecDeque::new();
        let mut after_remaining: u16 = 0;
        let mut response_bytes: usize = 0;
        let mut scanned_bytes: u64 = 0;
        let mut stop = GrepStop::RangeExhausted;
        let mut resume: Option<HistoryPosition> = None;

        'segments: for segment in &committed.segments {
            let Some((seg_first, seg_last)) = segment.stream_range() else {
                continue;
            };
            if seg_last <= start.line() || seg_first > scan_end {
                continue;
            }
            let scan = scan_committed(segment, &self.root, &committed.identity).await?;
            let mut index = 0usize;
            while index < scan.report.frames.len() {
                let line = scan.report.frames[index].header.line;
                let group_end = scan.report.frames[index..]
                    .iter()
                    .position(|frame| frame.header.line != line)
                    .map_or(scan.report.frames.len(), |offset| index + offset);
                let group: Vec<&ScannedFrame> =
                    scan.report.frames[index..group_end].iter().collect();
                index = group_end;
                if line < start.line() {
                    continue;
                }
                if line > scan_end {
                    break 'segments;
                }
                if scanned_bytes >= limits.scan_bytes() {
                    stop = GrepStop::ScanBudget;
                    resume = Some(pos(line, 0)?);
                    break 'segments;
                }

                let first_line = line == start.line();
                let mut line_from = if first_line { start.byte_offset() } else { 0 };
                let mut line_text = String::new();
                let mut line_truncated = false;
                let mut line_matches: Vec<(u64, u64)> = Vec::new();
                // A match is never reported twice: within a page the
                // non-overlap reset guarantees it, and across pages the
                // resume offset does. The preload completes the window so a
                // match straddling the resume offset is still found.
                matcher.set_report_from(if first_line { start.byte_offset() } else { 0 });
                if first_line && line_from > 0 {
                    let keep = matcher.preload_bytes();
                    let preceding = preceding_text(&group, &scan.data, line_from, keep)?;
                    let preload_from = line_from - preceding.len() as u64;
                    matcher.preload(&preceding, preload_from);
                }

                for frame in &group {
                    let payload = payload_str(frame, &scan.data)?;
                    scanned_bytes += payload.len() as u64;
                    let chunk_start = frame.header.line_offset;
                    let chunk_len = payload.len() as u64;
                    let from = line_from.max(chunk_start);
                    if from >= chunk_start + chunk_len {
                        continue;
                    }
                    let local = ceil_boundary(payload, (from - chunk_start) as usize);
                    let base = chunk_start + local as u64;
                    let text = &payload[local..];
                    line_from = base;
                    if context_lines > 0 {
                        append_capped(&mut line_text, text, &mut line_truncated);
                    }
                    for (match_start, match_end) in matcher.feed(text, base) {
                        if matches.len() as u32 >= limits.max_matches()
                            || response_bytes + MATCH_RESPONSE_BYTES > limits.max_bytes() as usize
                        {
                            // Stop *before* recording: the resume point is
                            // the last reported byte, so this match is
                            // found again — never lost, never duplicated.
                            stop = GrepStop::ResponseBudget;
                            resume = Some(pos(line, line_from)?);
                            break 'segments;
                        }
                        matches.push(GrepMatch::new(
                            pos(line, match_start)?,
                            match_end - match_start,
                        )?);
                        response_bytes += MATCH_RESPONSE_BYTES;
                        line_matches.push((match_start, match_end));
                        line_from = match_end;
                    }
                }

                if !line_matches.is_empty() {
                    for (context_line, text, truncated) in &ring {
                        if contexts
                            .last()
                            .is_some_and(|last| last.line() == *context_line)
                        {
                            continue;
                        }
                        push_context(
                            &mut contexts,
                            &mut response_bytes,
                            &mut contexts_truncated,
                            *context_line,
                            text,
                            *truncated,
                            &limits,
                        )?;
                    }
                    after_remaining = context_lines;
                } else if after_remaining > 0 {
                    push_context(
                        &mut contexts,
                        &mut response_bytes,
                        &mut contexts_truncated,
                        line,
                        &line_text,
                        line_truncated,
                        &limits,
                    )?;
                    after_remaining -= 1;
                }
                if context_lines > 0 {
                    ring.push_back((line, line_text, line_truncated));
                    while ring.len() > context_lines as usize {
                        ring.pop_front();
                    }
                }
            }
        }

        let scanned_latest = match resume {
            Some(position) => max_position(start, position),
            None => max_position(start, pos(scan_end, 0)?),
        };
        let scanned =
            HistoryRange::new(start, scanned_latest).map_err(|error| invalid(error.to_string()))?;
        let next_scan = match resume {
            Some(position) => Some(
                GrepScanPoint::new(query.clone(), position)
                    .map_err(|error| invalid(error.to_string()))?,
            ),
            None => None,
        };
        let page = GrepPage::new(
            matches,
            contexts,
            scanned,
            next_scan,
            stop,
            contexts_truncated,
            degraded,
        )
        .map_err(|error| invalid(error.to_string()))?;
        Ok(page)
    }

    /// Load and validate one log's committed normalized history.
    async fn committed_normalized(&self, log: &LogIdentity) -> Result<Committed, StorageError> {
        let ident = ResolvedIdentity::parse(log)?;
        let terminal = self
            .store
            .terminal(&ident.key)
            .await?
            .ok_or_else(|| StorageError::UnknownLog(ident.terminal_id.clone()))?;
        // A different epoch means old positions are no longer
        // interpretable; the position is refused, never re-anchored.
        if terminal.log_epoch != ident.header.epoch {
            return Err(expired(None, None));
        }
        if terminal.terminal_uuid != ident.header.terminal_uuid {
            return Err(invalid(
                "requested terminal identity does not match the persisted record",
            ));
        }
        let mut segments: Vec<SegmentRow> = self
            .store
            .segments(&ident.key, LogStream::Normalized)
            .await?
            .into_iter()
            .filter(|segment| {
                (segment.state == "active" || segment.state == "sealed")
                    && segment.stream_range().is_some()
            })
            .collect();
        segments.sort_by_key(|segment| {
            let (start, _) = segment
                .stream_range()
                .expect("filtered to non-empty ranges");
            (start, segment.segment_id)
        });
        let gaps = self.store.gaps(&ident.key, LogStream::Normalized).await?;
        // The ordered cross-segment chain must hold as a whole: an overlap,
        // a range past the watermark, or an uncovered hole refuses the read
        // until recovery ran (never silently narrowed away).
        let links: Vec<ChainLink> = segments
            .iter()
            .map(|segment| {
                let (start, end) = segment.stream_range().expect("filtered");
                ChainLink {
                    segment_id: segment.segment_id,
                    start,
                    end,
                }
            })
            .collect();
        validate_chain(
            LogStream::Normalized,
            terminal.retained_first_line,
            terminal.line_watermark,
            &links,
            &gaps,
        )
        .map_err(|violation| recovery(violation.detail(LogStream::Normalized)))?;
        Ok(Committed {
            identity: ident.header,
            terminal,
            gaps,
            segments,
        })
    }
}

/// Append `text` to a context line while its bound lasts, flagging a
/// shortened line instead of silently truncating it.
fn append_capped(target: &mut String, text: &str, truncated: &mut bool) {
    let room = CONTEXT_LINE_MAX_BYTES.saturating_sub(target.len());
    if text.len() <= room {
        target.push_str(text);
        return;
    }
    let take = floor_boundary(text, room);
    if take < text.len() {
        target.push_str(&text[..take]);
        *truncated = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn push_context(
    contexts: &mut Vec<GrepContext>,
    response_bytes: &mut usize,
    dropped: &mut bool,
    line: u64,
    text: &str,
    truncated: bool,
    limits: &GrepLimits,
) -> Result<(), StorageError> {
    if *dropped {
        return Ok(());
    }
    if *response_bytes + text.len() > limits.max_bytes() as usize {
        *dropped = true;
        return Ok(());
    }
    contexts
        .push(GrepContext::new(line, text, truncated).map_err(|error| invalid(error.to_string()))?);
    *response_bytes += text.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(matcher: &mut LiteralMatcher, chunks: &[&str]) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut offset = 0u64;
        for chunk in chunks {
            out.extend(matcher.feed(chunk, offset));
            offset += chunk.len() as u64;
        }
        out
    }

    #[test]
    fn literal_matcher_finds_matches_across_chunk_boundaries() {
        // A needle split by a chunk boundary is still found exactly once.
        let mut matcher = LiteralMatcher::new("needle", true);
        let found = feed_all(&mut matcher, &["nee", "dle", "!needle"]);
        assert_eq!(found, vec![(0, 6), (7, 13)]);

        // Arbitrary chunk splits never change the answer.
        let text = "abcdefabcdef";
        let whole = feed_all(&mut LiteralMatcher::new("cde", true), &[text]);
        assert_eq!(whole, vec![(2, 5), (8, 11)]);
        for split in 1..text.len() {
            let (a, b) = text.split_at(split);
            let split_found = feed_all(&mut LiteralMatcher::new("cde", true), &[a, b]);
            assert_eq!(split_found, whole, "split at {split}");
        }
    }

    #[test]
    fn literal_matcher_is_bounded_and_handles_long_lines() {
        let mut matcher = LiteralMatcher::new("abcd", true);
        let mut found = Vec::new();
        let mut offset = 0u64;
        for _ in 0..1024 {
            let chunk = format!("{}abcd", "x".repeat(1020));
            found.extend(matcher.feed(&chunk, offset));
            offset += chunk.len() as u64;
            assert!(
                matcher.window.len() <= matcher.needle.len(),
                "the window never exceeds the needle"
            );
            assert!(matcher.chars.len() <= matcher.needle.len());
        }
        assert_eq!(found.len(), 1024);
        assert_eq!(found[0].1 - found[0].0, 4);
    }

    #[test]
    fn literal_matcher_handles_case_folding_and_wide_text() {
        // Case-insensitive folding applies to text and needle alike.
        let mut matcher = LiteralMatcher::new("Wörld", false);
        assert_eq!(feed_all(&mut matcher, &["hello WÖR"]), Vec::new());
        let found = feed_all(&mut matcher, &["hello WÖR", "LD 世界"]);
        assert_eq!(found, vec![(6, 12)]);
        assert_eq!(&"hello WÖRLD 世界"[6..12], "WÖRLD");

        // A narrow needle still matches inside CJK text without splitting
        // characters.
        let mut matcher = LiteralMatcher::new("世界", true);
        assert_eq!(feed_all(&mut matcher, &["你好", "世界"]), vec![(6, 12)]);
    }

    #[test]
    fn literal_matcher_preload_reports_a_straddling_match_once() {
        // The page resumes at byte 3 of "neeneedle"; the preload supplies
        // the preceding bytes, so the straddling match is found.
        let mut matcher = LiteralMatcher::new("needle", true);
        matcher.preload("nee", 0);
        matcher.set_report_from(3);
        assert_eq!(matcher.feed("needle", 3), vec![(3, 9)]);

        // A match starting before the resume offset was already reported
        // and is not reported again.
        let mut matcher = LiteralMatcher::new("aaa", true);
        matcher.preload("aa", 0);
        matcher.set_report_from(2);
        assert_eq!(matcher.feed("aa", 2), Vec::new());
    }

    #[test]
    fn slice_line_is_rune_aligned_and_pages_by_offset() {
        let text = "你好世界ab";
        let frame = test_frame(1, 0, 0, text.as_bytes());
        let sliced = slice_line(&[&frame], text.as_bytes(), 0, 1024).expect("slice");
        assert_eq!(sliced.text, text);
        assert_eq!(sliced.start_offset, 0);
        assert!(!sliced.cut);

        let sliced = slice_line(&[&frame], text.as_bytes(), 6, 1024).expect("slice");
        assert_eq!(sliced.text, "世界ab");
        assert_eq!(sliced.start_offset, 6);

        // A byte budget inside a wide character walks forward, never
        // splitting it.
        let sliced = slice_line(&[&frame], text.as_bytes(), 0, 7).expect("slice");
        assert_eq!(sliced.text, "你好");
        assert!(sliced.cut);
        assert_eq!(sliced.resume_offset, 6);

        // An offset inside a character is clamped forward.
        let sliced = slice_line(&[&frame], text.as_bytes(), 1, 1024).expect("slice");
        assert_eq!(sliced.text, "好世界ab");
        assert_eq!(sliced.start_offset, 3);

        // An offset past the line clamps to its end.
        let sliced = slice_line(&[&frame], text.as_bytes(), 999, 1024).expect("slice");
        assert_eq!(sliced.text, "");
        assert_eq!(sliced.start_offset, text.len() as u64);
        assert!(!sliced.cut);
    }

    #[test]
    fn slice_line_stitches_frames_of_one_line() {
        let first = "你好";
        let second = "world";
        let first_frame = test_frame(4, 0, 0, first.as_bytes());
        let second_frame = test_frame(4, first.len() as u64, first.len(), second.as_bytes());
        let mut data = Vec::new();
        data.extend_from_slice(first.as_bytes());
        data.extend_from_slice(second.as_bytes());
        let sliced = slice_line(&[&first_frame, &second_frame], &data, 0, 1024).expect("slice");
        assert_eq!(sliced.text, "你好world");
        let sliced = slice_line(&[&first_frame, &second_frame], &data, 3, 1024).expect("slice");
        assert_eq!(sliced.text, "好world");
        assert_eq!(sliced.start_offset, 3);
    }

    #[test]
    fn boundary_helpers_never_split_a_character() {
        let text = "aé世";
        assert_eq!(ceil_boundary(text, 0), 0);
        // Byte 2 is inside the two-byte `é`, so the next boundary is 3.
        assert_eq!(ceil_boundary(text, 2), 3);
        assert_eq!(ceil_boundary(text, 3), 3);
        assert_eq!(floor_boundary(text, 3), 3);
        assert_eq!(floor_boundary(text, 2), 1);
        assert_eq!(floor_boundary(text, 100), text.len());
    }

    /// A minimal scanned-frame fixture: `data` is the concatenated payload
    /// buffer it indexes into.
    fn test_frame(line: u64, line_offset: u64, payload_at: usize, payload: &[u8]) -> ScannedFrame {
        ScannedFrame {
            header: crate::frame::FrameHeader {
                kind: crate::frame::FRAME_KIND_LINE,
                flags: crate::frame::FRAME_FLAG_LINE_END,
                frame_seq: 0,
                line,
                line_offset,
                payload_len: payload.len() as u32,
            },
            payload_at,
            end: payload_at + payload.len() + 4,
        }
    }
}
