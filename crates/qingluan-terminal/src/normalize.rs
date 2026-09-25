//! Bounded, streaming line normalization of one terminal's output.
//!
//! `vte` parses the byte stream into text and control functions; this
//! module turns that into **history lines** with the confirmed bounded
//! semantics: LF finalizes the pending line, CR/BS/HT and the supported
//! in-line cursor movement and erase functions edit it, style and OSC/DCS
//! payloads are discarded, and *nothing* here ever invents a line break.
//! In particular display-width wrapping is not modelled at all: a line
//! that would wrap on a screen stays exactly one history line.
//!
//! What is bounded, and how:
//!
//! - The **mutable tail** keeps at most [`TAIL_MAX_BYTES`] bytes and
//!   [`TAIL_MAX_CELLS`] display cells, evicting whole cells from the front.
//!   Evicted bytes are counted in the tail's absolute in-line offset, and
//!   the first eviction of a tail retires the line number that content
//!   would have used as an **explicit normalized-stream loss** (the
//!   writer's `record_line_loss`): the prefix is declared missing instead
//!   of being persisted as the start of a line that never began there. The
//!   retained suffix continues under the next line number, so numbering
//!   stays monotonic and is never reused. One retirement declares the whole
//!   unreadable prefix of that logical line — the exact retained start is
//!   reported by [`TailSnapshot`] while the tail is mutable, and a
//!   line-granular gap is the only granularity the storage format has.
//! - The **parser** keeps only vte's own partial-escape/partial-UTF-8
//!   state, so a control sequence or a multi-byte character split across
//!   two reads is handled by the parser, not by a growing buffer.
//! - The **pending queue** holds finalized lines whose frames are not yet
//!   durable; the sink drains it after every read, so it is bounded by one
//!   read plus one finalized tail.
//!
//! Every finalization is exactly once: a line is handed to the sink when it
//! ends (LF), when the output ends (a non-empty tail), or when the input
//! handoff drops bytes (a discontinuity — the real pending bytes are
//! committed, and the *unknown* loss is left to the terminal's degraded
//! latch instead of a fabricated line count).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use qingluan_core::terminal::{HistoryPosition, LogIdentity, TailId, TailPosition, TailSnapshot};
use unicode_width::UnicodeWidthChar;

use crate::limits::{TAB_WIDTH, TAIL_MAX_BYTES, TAIL_MAX_CELLS};

/// How many finalized tail mappings are retained before an old tail
/// position is explicitly expired.
const TAIL_MAPPING_CAPACITY: usize = 64;

/// One display cell: a base character plus the zero-width marks attached
/// to it.
#[derive(Debug, Clone)]
struct Cell {
    base: char,
    /// Combining and other zero-width marks attached to `base`, in order.
    marks: String,
    /// Display columns of the cell: 1, or 2 for a wide base.
    columns: u8,
}

impl Cell {
    fn new(base: char, columns: u8, marks: String) -> Self {
        Self {
            base,
            marks,
            columns,
        }
    }

    fn text_len(&self) -> usize {
        self.base.len_utf8() + self.marks.len()
    }

    fn write_into(&self, out: &mut String) {
        out.push(self.base);
        out.push_str(&self.marks);
    }
}

/// The mutable, not-yet-finalized line.
struct TailLine {
    cells: VecDeque<Cell>,
    /// Sum of `cell.text_len()` over `cells`, kept incrementally so the byte
    /// bound never needs a full scan.
    bytes: usize,
    /// Cursor as a cell index; it may sit past the last cell (blank space a
    /// later write fills with spaces).
    cursor: usize,
    /// Leading zero-width marks that have no base character yet.
    pending_marks: String,
}

impl TailLine {
    fn new() -> Self {
        Self {
            cells: VecDeque::new(),
            bytes: 0,
            cursor: 0,
            pending_marks: String::new(),
        }
    }

    /// Display columns used by the cells before `index`.
    fn columns_up_to(&self, index: usize) -> usize {
        self.cells
            .iter()
            .take(index)
            .map(|cell| cell.columns as usize)
            .sum()
    }

    /// The cell index reached by `columns` display columns; past the end it
    /// is that many blank columns further.
    fn index_at_column(&self, columns: usize) -> usize {
        let mut used = 0usize;
        for (index, cell) in self.cells.iter().enumerate() {
            if used + cell.columns as usize > columns {
                return index;
            }
            used += cell.columns as usize;
        }
        self.cells.len() + columns.saturating_sub(used)
    }

    fn push_space(&mut self) {
        self.cells.push_back(Cell::new(' ', 1, String::new()));
        self.bytes += 1;
    }

    /// Pad with space cells up to the cursor.
    fn pad_to_cursor(&mut self) {
        while self.cells.len() < self.cursor {
            self.push_space();
        }
    }

    /// Write `base` at the cursor, overwriting the cell there or appending
    /// when the cursor is past the end.
    fn write(&mut self, base: char, columns: u8) {
        self.pad_to_cursor();
        let marks = std::mem::take(&mut self.pending_marks);
        let cell = Cell::new(base, columns, marks);
        let added = cell.text_len();
        if self.cursor < self.cells.len() {
            let removed = self.cells[self.cursor].text_len();
            self.cells[self.cursor] = cell;
            self.bytes = self.bytes + added - removed;
        } else {
            self.cells.push_back(cell);
            self.bytes += added;
        }
        self.cursor += 1;
    }

    /// Attach a zero-width mark to the cell before the cursor, or hold it
    /// for the next base character when there is none.
    fn attach_mark(&mut self, mark: char) {
        let added = mark.len_utf8();
        if self.cursor > 0 && self.cursor <= self.cells.len() {
            if let Some(cell) = self.cells.get_mut(self.cursor - 1) {
                cell.marks.push(mark);
                self.bytes += added;
                return;
            }
        }
        self.pending_marks.push(mark);
        self.bytes += added;
    }

    /// Replace the cells in `[from, to)` with space cells, keeping the cell
    /// count: erasing clears text, it does not shift the line.
    fn erase(&mut self, from: usize, to: usize) {
        let to = to.min(self.cells.len());
        for index in from..to {
            let removed = self.cells[index].text_len();
            self.cells[index] = Cell::new(' ', 1, String::new());
            self.bytes = self.bytes + 1 - removed;
        }
    }

    /// Drop everything from `from` to the end of the line.
    fn truncate_from(&mut self, from: usize) {
        while self.cells.len() > from {
            let removed = self.cells.pop_back().expect("non-empty").text_len();
            self.bytes -= removed;
        }
        self.cursor = self.cursor.min(self.cells.len());
    }

    fn render(&self) -> String {
        let mut out = String::with_capacity(self.bytes);
        for cell in &self.cells {
            cell.write_into(&mut out);
        }
        out
    }
}

/// One finalized tail line: the mapping that lets an old tail position
/// continue on the stable history line it became.
#[derive(Debug, Clone)]
pub(crate) struct FinalizedTail {
    tail_id: TailId,
    revision: u64,
    line: u64,
    omitted_prefix_bytes: u64,
}

/// The shared mutable tail of one terminal.
///
/// The sink mutates it while it parses; the runtime reads snapshots and
/// resolves old tail positions. Every mutation bumps `revision`, so a reader
/// that held an older revision is explicitly expired rather than silently
/// shown newer content.
pub(crate) struct TailState {
    log: LogIdentity,
    tail_id: TailId,
    revision: u64,
    /// Bytes evicted from the front of the current logical line.
    omitted_prefix_bytes: u64,
    line: Option<TailLine>,
    /// Line number the current tail content will use when finalized.
    line_number: u64,
    /// Whether the current tail already retired a line number for its
    /// omitted prefix.
    retired_once: bool,
    /// The retired line number, waiting for the sink to record it.
    retired: Option<u64>,
    /// Mappings of finalized lines whose durability is **not proven yet**,
    /// in line order. They resolve to nothing: a finalized line that is only
    /// buffered by the writer has no readable history line to point at.
    pending: VecDeque<FinalizedTail>,
    /// Mappings of lines proven durable (committed, or closed after being
    /// explicitly recorded as dropped), oldest first (bounded).
    finalized: VecDeque<FinalizedTail>,
}

impl TailState {
    pub(crate) fn new(log: LogIdentity, tail_id: TailId, line_number: u64) -> Self {
        Self {
            log,
            tail_id,
            revision: 0,
            omitted_prefix_bytes: 0,
            line: None,
            line_number,
            retired_once: false,
            retired: None,
            pending: VecDeque::new(),
            finalized: VecDeque::new(),
        }
    }

    /// A snapshot of the current tail revision, with an omitted prefix
    /// flagged instead of hidden.
    pub(crate) fn snapshot(&self) -> TailSnapshot {
        let text = self.line.as_ref().map(TailLine::render).unwrap_or_default();
        TailSnapshot::new(
            self.log.clone(),
            TailPosition::new(
                self.tail_id.clone(),
                self.revision,
                self.omitted_prefix_bytes,
            ),
            text,
            self.omitted_prefix_bytes > 0,
        )
    }

    /// Resolve an old tail position onto the stable history line it became.
    ///
    /// A still-live revision, an overwritten one, an unknown tail, a mapping
    /// whose durability is not proven yet (the line is buffered, or its
    /// batch failed), a mapping already evicted, or an offset inside the
    /// omitted prefix resolves to `None` — an explicit failure, never a
    /// silent splice onto newer content or onto a line that is not
    /// readable.
    pub(crate) fn resolve(&self, position: &TailPosition) -> Option<HistoryPosition> {
        if position.tail_id() == &self.tail_id && position.revision() == self.revision {
            // Still the mutable tail: it has no stable history line yet.
            return None;
        }
        let entry = self.finalized.iter().find(|entry| {
            &entry.tail_id == position.tail_id() && entry.revision == position.revision()
        })?;
        if position.byte_offset() < entry.omitted_prefix_bytes {
            return None;
        }
        HistoryPosition::new(
            entry.line,
            position.byte_offset() - entry.omitted_prefix_bytes,
        )
        .ok()
    }

    /// The line number retired by an eviction, if the sink has not realized
    /// it yet.
    pub(crate) fn take_retired(&mut self) -> Option<u64> {
        self.retired.take()
    }

    /// Publish one mapping whose line is proven durable.
    fn publish(&mut self, entry: FinalizedTail) {
        self.finalized.push_back(entry);
        while self.finalized.len() > TAIL_MAPPING_CAPACITY {
            self.finalized.pop_front();
        }
    }

    /// A durability proof arrived for the normalized stream: every mapping
    /// still waiting for one now resolves to its history line, in line
    /// order and under the same capacity bound.
    fn mappings_committed(&mut self) {
        while let Some(entry) = self.pending.pop_front() {
            self.publish(entry);
        }
    }

    /// The waiting batch was durably dropped as an explicit gap: its lines
    /// do not exist, so its mappings expire and can never resolve.
    fn mappings_dropped(&mut self) {
        self.pending.clear();
    }

    /// Whether any finalized mapping is still waiting for a durability proof.
    fn has_pending_mappings(&self) -> bool {
        !self.pending.is_empty()
    }

    fn current(&mut self) -> &mut TailLine {
        self.line.get_or_insert_with(TailLine::new)
    }

    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn write_char(&mut self, c: char) {
        match c.width() {
            // Zero-width marks attach to the character they follow (or wait
            // for the next base character). They never advance the cursor,
            // so a wide character followed by a combining mark stays one
            // cell.
            Some(0) => self.current().attach_mark(c),
            Some(width) => self.current().write(c, width.clamp(1, 2) as u8),
            None => self.current().write(c, 1),
        }
        self.bump();
        self.enforce_bounds();
    }

    fn carriage_return(&mut self) {
        self.current().cursor = 0;
        self.bump();
    }

    fn backspace(&mut self) {
        let cursor = self.current().cursor;
        self.current().cursor = cursor.saturating_sub(1);
        self.bump();
    }

    fn tab(&mut self) {
        let line = self.current();
        let current = line.columns_up_to(line.cursor);
        let target = (current / TAB_WIDTH + 1) * TAB_WIDTH;
        line.cursor = line.index_at_column(target);
        line.pad_to_cursor();
        self.bump();
        self.enforce_bounds();
    }

    fn move_columns(&mut self, columns: u64, forward: bool) {
        let bounded = usize::try_from(columns.min(TAIL_MAX_CELLS as u64)).unwrap_or(TAIL_MAX_CELLS);
        let line = self.current();
        let current = line.columns_up_to(line.cursor);
        let target = if forward {
            current.saturating_add(bounded)
        } else {
            current.saturating_sub(bounded)
        };
        line.cursor = line.index_at_column(target).min(TAIL_MAX_CELLS);
        self.bump();
    }

    fn move_to_column(&mut self, column: u64) {
        // CSI G and CSI ` are 1-based columns.
        let target = usize::try_from(column.saturating_sub(1).min(TAIL_MAX_CELLS as u64))
            .unwrap_or(TAIL_MAX_CELLS);
        let line = self.current();
        line.cursor = line.index_at_column(target).min(TAIL_MAX_CELLS);
        self.bump();
    }

    fn erase_in_line(&mut self, mode: u16) {
        let line = self.current();
        let cursor = line.cursor;
        match mode {
            0 => line.truncate_from(cursor),
            1 => line.erase(0, cursor),
            2 => line.truncate_from(0),
            // An unknown mode is not a line edit; never guess.
            _ => return,
        }
        self.bump();
    }

    fn erase_chars(&mut self, count: u64) {
        let bounded = usize::try_from(count.min(TAIL_MAX_CELLS as u64)).unwrap_or(TAIL_MAX_CELLS);
        let line = self.current();
        let from = line.cursor;
        let to = from.saturating_add(bounded).min(line.cells.len());
        line.erase(from, to);
        self.bump();
    }

    /// The first eviction of a tail retires the line number its content
    /// would have used: the dropped prefix is declared missing in one
    /// explicit gap instead of being presented as the start of a line that
    /// never began there. Later evictions of the same logical line are part
    /// of that same declared-missing prefix, so they retire nothing more.
    fn enforce_bounds(&mut self) {
        let Some(line) = self.line.as_mut() else {
            return;
        };
        while line.bytes > TAIL_MAX_BYTES || line.cells.len() > TAIL_MAX_CELLS {
            let Some(front) = line.cells.pop_front() else {
                break;
            };
            let bytes = front.text_len();
            line.bytes = line.bytes.saturating_sub(bytes);
            line.cursor = line.cursor.saturating_sub(1);
            self.omitted_prefix_bytes += bytes as u64;
            if !self.retired_once {
                self.retired_once = true;
                self.retired = Some(self.line_number);
                self.line_number = self.line_number.saturating_add(1);
            }
        }
    }

    /// Take the current tail's text, if it has any content, and prepare the
    /// mapping that becomes valid once the sink persisted it.
    fn finalize(&mut self, cause: Finalize) -> Option<(FinalizedTail, String)> {
        let line = match self.line.take() {
            Some(line) => line,
            None if cause == Finalize::Separator => TailLine::new(),
            None => return None,
        };
        let text = line.render();
        if text.is_empty() && self.omitted_prefix_bytes == 0 && cause != Finalize::Separator {
            // The end of output and a discontinuity do not fabricate an
            // empty line. A real LF/VT/FF does: the separator fixes an empty
            // history line even when no printable byte preceded it.
            return None;
        }
        let entry = FinalizedTail {
            tail_id: self.tail_id.clone(),
            revision: self.revision,
            line: self.line_number,
            omitted_prefix_bytes: self.omitted_prefix_bytes,
        };
        self.line_number = self.line_number.saturating_add(1);
        self.omitted_prefix_bytes = 0;
        self.retired_once = false;
        self.retired = None;
        self.pending.push_back(entry.clone());
        self.bump();
        Some((entry, text))
    }
}

/// Why a pending tail is being finalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finalize {
    /// A real LF/VT/FF separator ended the line: an empty pending line is a
    /// real empty history line, and it consumes its number.
    Separator,
    /// The output ended (or the handoff dropped bytes): nothing is
    /// fabricated, so an empty pending tail commits no line.
    EndOfOutput,
}

/// One finalized line or explicit loss the sink must persist, in order.
pub(crate) enum PendingLine {
    /// A finalized history line and its stable number.
    Line { line: u64, text: String },
    /// Retired line numbers that were never written: an explicit loss.
    Loss { first_line: u64, lines: u64 },
}

/// The streaming normalizer of one output sink.
pub(crate) struct Normalizer {
    parser: vte::Parser,
    state: Arc<Mutex<TailState>>,
    ready: VecDeque<PendingLine>,
}

impl Normalizer {
    pub(crate) fn new(state: Arc<Mutex<TailState>>) -> Self {
        Self {
            parser: vte::Parser::new(),
            state,
            ready: VecDeque::new(),
        }
    }

    /// Feed one read's bytes. Never awaits and never blocks: everything the
    /// parser does to the tail happens under the tail lock for the duration
    /// of this call only.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        let mut performer = Performer {
            state: self.state.lock().expect("tail state"),
            ready: VecDeque::new(),
        };
        self.parser.advance(&mut performer, bytes);
        self.ready.append(&mut performer.ready);
    }

    /// End of output: a non-empty tail becomes exactly one history line.
    pub(crate) fn finish(&mut self) {
        let mut state = self.state.lock().expect("tail state");
        drain_finalization(&mut state, &mut self.ready, Finalize::EndOfOutput);
    }

    /// The output handoff dropped an unknown run of bytes: the real pending
    /// bytes are committed as one line (they were really written), and the
    /// loss itself is left to the terminal's explicit degraded latch —
    /// never to a fabricated normalized line count, which the dropped bytes
    /// cannot determine.
    pub(crate) fn discontinuity(&mut self) {
        let mut state = self.state.lock().expect("tail state");
        drain_finalization(&mut state, &mut self.ready, Finalize::EndOfOutput);
    }

    /// A snapshot of the mutable tail at the caller's own ordering point.
    /// The output sink is the only mutator, so a caller inside that task
    /// sees a cut that no later byte can enter.
    pub(crate) fn snapshot(&self) -> TailSnapshot {
        self.state.lock().expect("tail state").snapshot()
    }

    /// The next item the sink must persist, in the order it was produced.
    ///
    /// A finalized line's tail mapping stays private inside the tail state
    /// until a durability proof arrives ([`Normalizer::mappings_committed`]),
    /// so a caller can never resolve a tail position onto a line the writer
    /// has merely accepted.
    pub(crate) fn next_pending(&mut self) -> Option<PendingLine> {
        self.ready.pop_front()
    }

    /// A durability proof for the normalized stream arrived: the mappings of
    /// every line accepted so far now resolve.
    pub(crate) fn mappings_committed(&mut self) {
        self.state.lock().expect("tail state").mappings_committed();
    }

    /// The normalized batch was durably dropped as an explicit gap: its
    /// mappings expire, because their lines do not exist.
    pub(crate) fn mappings_dropped(&mut self) {
        self.state.lock().expect("tail state").mappings_dropped();
    }

    /// Whether any finalized mapping is still waiting for a durability proof.
    pub(crate) fn has_pending_mappings(&self) -> bool {
        self.state
            .lock()
            .expect("tail state")
            .has_pending_mappings()
    }
}

/// Queue the pending tail's retirement (if any) and its finalization, in
/// that order: the retired number must be recorded before the suffix line
/// that continues after it can be appended.
fn drain_finalization(state: &mut TailState, ready: &mut VecDeque<PendingLine>, cause: Finalize) {
    if let Some(retired) = state.take_retired() {
        ready.push_back(PendingLine::Loss {
            first_line: retired,
            lines: 1,
        });
    }
    if let Some((entry, text)) = state.finalize(cause) {
        ready.push_back(PendingLine::Line {
            line: entry.line,
            text,
        });
    }
}

/// One parse step's view of the tail: it holds the tail lock for the whole
/// `advance` call and queues whatever must be persisted.
struct Performer<'a> {
    state: std::sync::MutexGuard<'a, TailState>,
    ready: VecDeque<PendingLine>,
}

impl Performer<'_> {
    /// Queue a line number retired by an eviction, at the point the
    /// eviction happened: it must reach storage before any line that
    /// continues after it.
    fn sync_loss(&mut self) {
        if let Some(retired) = self.state.take_retired() {
            self.ready.push_back(PendingLine::Loss {
                first_line: retired,
                lines: 1,
            });
        }
    }
}

impl vte::Perform for Performer<'_> {
    fn print(&mut self, c: char) {
        self.state.write_char(c);
        self.sync_loss();
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // LF, VT, and FF all end the pending line. FF is treated as a
            // line break rather than a screen clear: this normalizer has no
            // screen, and dropping real output would be worse than one extra
            // boundary.
            0x0A | 0x0B | 0x0C => {
                drain_finalization(&mut self.state, &mut self.ready, Finalize::Separator)
            }
            // CR returns to the start of the line; later text overwrites in
            // place.
            0x0D => self.state.carriage_return(),
            0x08 => self.state.backspace(),
            0x09 => self.state.tab(),
            // BEL, SO/SI, and every other C0 byte change nothing about the
            // line's text.
            _ => {}
        }
        self.sync_loss();
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match action {
            // In-line cursor movement. A zero (or absent) parameter means
            // one step, per ECMA-48.
            'C' => self
                .state
                .move_columns(param(params, 1).max(1).into(), true),
            'D' => self
                .state
                .move_columns(param(params, 1).max(1).into(), false),
            'G' | '`' => self.state.move_to_column(param(params, 1).max(1).into()),
            // In-line erase. Here an explicit 0 is a real mode, so it must
            // not be folded into the default.
            'K' => {
                let mode = param(params, 0);
                self.state.erase_in_line(mode)
            }
            'J' => {
                // Without a screen there is no display to erase: ED is an
                // in-line erase (0 = to the end of the line, 1 = to its
                // start, 2 = the whole line).
                let mode = param(params, 0);
                self.state.erase_in_line(mode)
            }
            'X' => self.state.erase_chars(param(params, 1).max(1).into()),
            // SGR, modes, scroll regions, line insert/delete, and every other
            // sequence neither edit the line nor create history. Insert and
            // delete in particular must never rewrite numbered history.
            _ => {}
        }
        self.sync_loss();
    }

    // Style, title, clipboard, and device-control payloads are discarded:
    // they are not normalized line text, and none of them may create a line.
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
    }

    fn put(&mut self, _byte: u8) {}

    fn unhook(&mut self) {}

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}

/// The first CSI parameter, or `default` when the sequence omitted it.
///
/// An explicit zero is returned unchanged: for cursor movement and ECH a
/// zero means one step, but for EL/ED it is a real mode, so callers that
/// need the movement reading apply `.max(1)` themselves.
fn param(params: &vte::Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|group| group.first().copied())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingluan_core::terminal::{
        ExternalSessionId, LogEpoch, SessionRef, SessionSource, TerminalId, TerminalRef,
    };

    fn log() -> LogIdentity {
        LogIdentity {
            terminal: TerminalRef {
                session: SessionRef {
                    source: SessionSource::new("test"),
                    external_id: ExternalSessionId::new("s1"),
                },
                terminal_id: TerminalId::new("t1"),
            },
            log_epoch: LogEpoch::new("epoch"),
        }
    }

    fn state() -> Arc<Mutex<TailState>> {
        Arc::new(Mutex::new(TailState::new(log(), TailId::new("tail"), 1)))
    }

    fn tail_text(state: &Arc<Mutex<TailState>>) -> String {
        state.lock().unwrap().snapshot().text().to_owned()
    }

    /// Drain everything the normalizer has queued so far.
    fn drain(normalizer: &mut Normalizer) -> (Vec<(u64, String)>, Vec<(u64, u64)>) {
        let mut lines = Vec::new();
        let mut losses = Vec::new();
        collect(normalizer, &mut lines, &mut losses);
        (lines, losses)
    }

    /// Feed chunks, then the end of output, and return every finalized line
    /// and explicit loss in order.
    fn run(normalizer: &mut Normalizer, chunks: &[&[u8]]) -> (Vec<(u64, String)>, Vec<(u64, u64)>) {
        let mut lines = Vec::new();
        let mut losses = Vec::new();
        for chunk in chunks {
            normalizer.feed(chunk);
            collect(normalizer, &mut lines, &mut losses);
        }
        (lines, losses)
    }

    fn collect(
        normalizer: &mut Normalizer,
        lines: &mut Vec<(u64, String)>,
        losses: &mut Vec<(u64, u64)>,
    ) {
        while let Some(item) = normalizer.next_pending() {
            match item {
                PendingLine::Line { line, text } => lines.push((line, text)),
                PendingLine::Loss { first_line, lines } => losses.push((first_line, lines)),
            }
        }
    }

    fn run_to_end(
        normalizer: &mut Normalizer,
        chunks: &[&[u8]],
    ) -> (Vec<(u64, String)>, Vec<(u64, u64)>) {
        let (mut lines, mut losses) = run(normalizer, chunks);
        normalizer.finish();
        collect(normalizer, &mut lines, &mut losses);
        (lines, losses)
    }

    #[test]
    fn normalizes_lines_and_control_functions() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));

        let (lines, losses) = run(
            &mut normalizer,
            &[
                b"one\ntwo\rX\rY\nback\x08c\ntab\tT\n",
                "wide\u{4e00}\n".as_bytes(),
            ],
        );
        assert_eq!(
            lines,
            vec![
                (1, "one".to_owned()),
                (2, "Ywo".to_owned()),
                (3, "bacc".to_owned()),
                (4, "tab     T".to_owned()),
                (5, "wide\u{4e00}".to_owned()),
            ]
        );
        assert!(losses.is_empty());

        // LF, VT, and FF each end a line.
        let (lines, _) = run(&mut normalizer, &[b"a\x0bb\x0cc\n"]);
        assert_eq!(
            lines,
            vec![
                (6, "a".to_owned()),
                (7, "b".to_owned()),
                (8, "c".to_owned())
            ]
        );
    }

    #[test]
    fn backspace_and_tab_edit_the_pending_line() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"ab\x08c");
        assert_eq!(tail_text(&state), "ac");
        normalizer.feed(b"\tX");
        assert_eq!(tail_text(&state), "ac      X");
        normalizer.feed(b"\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08Q");
        assert_eq!(tail_text(&state), "Qc      X");
    }

    #[test]
    fn in_line_cursor_movement_and_erase() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));

        // CSI 3 G moves to column 3 (1-based) and overwrites that cell.
        normalizer.feed(b"abcdef\x1b[3GX");
        assert_eq!(tail_text(&state), "abXdef");
        // CSI 2 C advances two columns and the next write lands there, so
        // the character in between is kept: `abXde-`.
        normalizer.feed(b"\x1b[2C-");
        assert_eq!(tail_text(&state), "abXde-");
        // CSI 1 D steps back one column.
        normalizer.feed(b"\x1b[1DZ");
        assert_eq!(tail_text(&state), "abXdeZ");
        // CSI 0 K erases from the cursor to the end of the line: the cursor
        // sits past the last cell, so nothing changes.
        normalizer.feed(b"\x1b[0K");
        assert_eq!(tail_text(&state), "abXdeZ");
        // Moving to the middle and erasing removes the tail of the line.
        normalizer.feed(b"\x1b[4G\x1b[0K");
        assert_eq!(tail_text(&state), "abX");
        // CSI 2 K erases the whole line.
        normalizer.feed(b"\x1b[2K");
        assert_eq!(tail_text(&state), "");
        // CSI 1 G returns to column 1; CSI 3 X erases three cells in place.
        normalizer.feed(b"abcdef\x1b[1G\x1b[3X");
        assert_eq!(tail_text(&state), "   def");
        // CSI 2 J (ED 2) is the in-line approximation of clearing the line.
        normalizer.feed(b"\x1b[2J");
        assert_eq!(tail_text(&state), "");
        // CSI 3 ` is a column move exactly like CSI G.
        normalizer.feed(b"abcdef\x1b[3`Y");
        assert_eq!(tail_text(&state), "abYdef");
    }

    #[test]
    fn style_modes_osc_and_dcs_never_touch_the_line() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"\x1b[1;31mred\x1b[0m\x1b[?25l\x1b[2;5r\x1b[1L\x1b[2M");
        assert_eq!(tail_text(&state), "red");
        normalizer.feed(b"\x1b]0;window title\x07\x1b]8;;http://example\x1b\\");
        assert_eq!(tail_text(&state), "red");
        normalizer.feed(b"\x1bPq#0;stuff\x1b\\\x07\x1b( B");
        assert_eq!(tail_text(&state), "red");
        // An invalid UTF-8 byte becomes the replacement character instead of
        // splitting the line.
        normalizer.feed(b"\xffz");
        assert_eq!(tail_text(&state), "red\u{fffd}z");
    }

    #[test]
    fn wide_and_combining_characters_are_preserved() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));

        // Chinese wide characters occupy two columns each, so a move to
        // column 3 lands on the second character.
        normalizer.feed("\u{4f60}\u{597d}\u{4e16}\u{754c}".as_bytes());
        assert_eq!(tail_text(&state), "你好世界");
        normalizer.feed(b"\x1b[3G");
        normalizer.feed(b"X");
        assert_eq!(tail_text(&state), "你X世界");

        // A decomposing combining mark stays attached to its base and does
        // not advance the cursor.
        normalizer.feed(b"\n");
        normalizer.feed("e\u{301}x".as_bytes());
        assert_eq!(tail_text(&state), "e\u{301}x");

        // A mark with no base yet joins the next base character.
        normalizer.feed(b"\n");
        normalizer.feed("\u{301}a".as_bytes());
        assert_eq!(tail_text(&state), "a\u{301}");
    }

    #[test]
    fn arbitrary_chunk_splits_do_not_change_the_result() {
        let input =
            "line\u{4e00}\u{1f600}\u{301}\rX\nsecond\x1b[2C\ntab\there\x1b[1P\ntail".as_bytes();
        let reference_state = state();
        let mut reference = Normalizer::new(Arc::clone(&reference_state));
        let (reference_lines, reference_losses) = run_to_end(&mut reference, &[input]);
        assert!(reference_losses.is_empty());

        // Every split of the input must produce the same history and the
        // same tail, because a control sequence or a multi-byte character
        // split across reads is the parser's business.
        for split in 0..=input.len() {
            let split_state = state();
            let mut normalizer = Normalizer::new(Arc::clone(&split_state));
            let (first, second) = input.split_at(split);
            let (lines, losses) = run_to_end(&mut normalizer, &[first, second]);
            assert!(losses.is_empty());
            assert_eq!(lines, reference_lines, "split at {split}");
            assert_eq!(
                tail_text(&split_state),
                tail_text(&reference_state),
                "split at {split}"
            );
        }
    }

    #[test]
    fn display_width_never_creates_a_history_line() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        // Far wider than any screen, with no LF anywhere.
        let wide = "x".repeat(8000);
        normalizer.feed(wide.as_bytes());
        let (lines, losses) = drain(&mut normalizer);
        assert!(lines.is_empty(), "wrapping is not a line break");
        assert!(losses.is_empty());
        assert_eq!(tail_text(&state).len(), 8000);

        let (lines, _) = run_to_end(&mut normalizer, &[]);
        assert_eq!(lines, vec![(1, wide)]);
    }

    #[test]
    fn an_over_long_tail_is_bounded_and_retires_one_line() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        let fed = TAIL_MAX_BYTES + 4096;
        normalizer.feed(&vec![b'z'; fed]);

        // The retained tail is bounded, its absolute offset is honest, and
        // exactly one line number is retired for the omitted prefix.
        let snapshot = state.lock().unwrap().snapshot();
        assert!(snapshot.text().len() <= TAIL_MAX_BYTES);
        assert!(snapshot.truncated());
        assert_eq!(
            snapshot.position().byte_offset(),
            (fed - snapshot.text().len()) as u64
        );
        let (lines, losses) = drain(&mut normalizer);
        assert!(lines.is_empty());
        assert_eq!(
            losses,
            vec![(1, 1)],
            "the omitted prefix is an explicit loss"
        );

        // The suffix continues under the next line number, and further
        // evictions of the same logical line retire nothing more.
        normalizer.feed(&vec![b'y'; TAIL_MAX_BYTES + 4096]);
        let (lines, losses) = drain(&mut normalizer);
        assert!(lines.is_empty());
        assert!(losses.is_empty(), "one retirement per logical line");
        let (lines, losses) = run_to_end(&mut normalizer, &[]);
        assert!(losses.is_empty());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, 2, "the suffix uses the next line number");
        assert!(lines[0].1.len() <= TAIL_MAX_BYTES);

        // A fresh logical line starts its own retirement budget.
        normalizer.feed(&vec![b'w'; fed]);
        let (_, losses) = drain(&mut normalizer);
        assert_eq!(losses, vec![(3, 1)]);
    }

    #[test]
    fn the_tail_cell_and_byte_bounds_hold_for_every_input() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        for _ in 0..64 {
            normalizer.feed(&vec![b'a'; 4096]);
        }
        {
            let state = state.lock().unwrap();
            let line = state.line.as_ref().expect("a pending tail");
            assert!(line.cells.len() <= TAIL_MAX_CELLS);
            assert!(line.bytes <= TAIL_MAX_BYTES);
        }
        // A huge cursor jump cannot grow the line either.
        normalizer.feed(b"\x1b[99999999G");
        normalizer.feed(b"x");
        normalizer.feed(b"\x1b[99999999C");
        {
            let state = state.lock().unwrap();
            let line = state.line.as_ref().expect("a pending tail");
            assert!(line.cells.len() <= TAIL_MAX_CELLS);
            assert!(line.bytes <= TAIL_MAX_BYTES);
            assert!(line.cursor <= TAIL_MAX_CELLS);
        }
    }

    #[test]
    fn finalization_is_exactly_once_and_empty_tails_commit_nothing() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));

        // An empty tail at end of output commits no line.
        let (lines, losses) = run_to_end(&mut normalizer, &[]);
        assert!(lines.is_empty());
        assert!(losses.is_empty());

        // A line ended by LF is not committed a second time by the end of
        // output, and the tail after it is.
        let (lines, _) = run(&mut normalizer, &[b"ended\n"]);
        assert_eq!(lines, vec![(1, "ended".to_owned())]);
        let (lines, _) = run_to_end(&mut normalizer, &[b"last"]);
        assert_eq!(lines, vec![(2, "last".to_owned())]);

        // A second end of output finds nothing.
        let (lines, losses) = run_to_end(&mut normalizer, &[]);
        assert!(lines.is_empty());
        assert!(losses.is_empty());
    }

    #[test]
    fn line_separators_preserve_empty_history_lines() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));

        let (lines, losses) = run_to_end(&mut normalizer, &[b"\n\ntext\n"]);
        assert_eq!(
            lines,
            vec![
                (1, String::new()),
                (2, String::new()),
                (3, "text".to_owned()),
            ]
        );
        assert!(losses.is_empty());
    }

    #[test]
    fn a_discontinuity_commits_real_bytes_and_never_fabricates_a_count() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"partial");
        normalizer.discontinuity();
        normalizer.feed(b"continued");
        let (lines, losses) = run_to_end(&mut normalizer, &[]);
        // The real bytes are committed on both sides of the loss; the loss
        // itself is the terminal's degraded latch, not a made-up line count.
        assert_eq!(
            lines,
            vec![(1, "partial".to_owned()), (2, "continued".to_owned())]
        );
        assert!(losses.is_empty());
    }

    #[test]
    fn an_old_tail_position_resolves_onto_its_history_line() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed("你好".as_bytes());
        let live = state.lock().unwrap().snapshot().position().clone();
        normalizer.feed(b"!");
        let final_position = state.lock().unwrap().snapshot().position().clone();
        normalizer.feed(b"\n");

        let item = normalizer.next_pending().expect("a finalized line");
        assert!(matches!(item, PendingLine::Line { line: 1, .. }));
        // Durability is proven (the sink appended and flushed), so the
        // mapping resolves from now on.
        {
            let guard = state.lock().unwrap();
            assert!(
                guard
                    .resolve(&TailPosition::new(
                        final_position.tail_id().clone(),
                        final_position.revision(),
                        3,
                    ))
                    .is_none(),
                "a finalized line is not resolvable before its durability is proven"
            );
        }
        normalizer.mappings_committed();
        let guard = state.lock().unwrap();

        // The finalized revision maps onto the line it became, offset by the
        // omitted prefix (none here).
        let resolved = guard
            .resolve(&TailPosition::new(
                final_position.tail_id().clone(),
                final_position.revision(),
                3,
            ))
            .expect("the finalized revision resolves");
        assert_eq!(resolved.line(), 1);
        assert_eq!(resolved.byte_offset(), 3);

        // A still-live revision, an overwritten one, an unknown tail, and an
        // offset in the omitted prefix are all explicit failures.
        assert!(guard.resolve(&live).is_none());
        assert!(
            guard
                .resolve(&TailPosition::new(final_position.tail_id().clone(), 0, 0))
                .is_none()
        );
        assert!(
            guard
                .resolve(&TailPosition::new(TailId::new("other"), 1, 0))
                .is_none()
        );
    }

    #[test]
    fn a_retired_prefix_offset_is_not_resolvable() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(&vec![b'a'; TAIL_MAX_BYTES + 1024]);
        let omitted = state.lock().unwrap().snapshot().position().byte_offset();
        assert!(omitted > 0);
        let position = state.lock().unwrap().snapshot().position().clone();
        normalizer.feed(b"\n");
        // The retirement and the suffix are both queued, in that order.
        let mut saw_suffix = false;
        while let Some(item) = normalizer.next_pending() {
            if matches!(item, PendingLine::Line { line: 2, .. }) {
                saw_suffix = true;
            }
        }
        assert!(saw_suffix, "the retained suffix is the next history line");
        normalizer.mappings_committed();
        let guard = state.lock().unwrap();

        // An offset inside the omitted prefix has no history position; one
        // inside the retained suffix does, shifted by the omission.
        assert!(
            guard
                .resolve(&TailPosition::new(
                    position.tail_id().clone(),
                    position.revision(),
                    0
                ))
                .is_none()
        );
        let resolved = guard
            .resolve(&TailPosition::new(
                position.tail_id().clone(),
                position.revision(),
                omitted + 5,
            ))
            .expect("the retained suffix resolves");
        assert_eq!(resolved.byte_offset(), 5);
    }

    #[test]
    fn one_terminal_has_one_stable_tail_id_and_monotonic_revisions() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"a\nb\nc");
        let snapshot = state.lock().unwrap().snapshot();
        assert_eq!(snapshot.position().tail_id().as_str(), "tail");
        assert!(snapshot.position().revision() >= 5);
    }

    #[test]
    fn a_mapping_resolves_only_after_a_durability_proof() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"one\n");
        let item = normalizer.next_pending().expect("a finalized line");
        assert!(matches!(item, PendingLine::Line { line: 1, .. }));

        // The tail revision that became line 1.
        let position = TailPosition::new(TailId::new("tail"), 3, 0);
        assert!(
            state.lock().unwrap().resolve(&position).is_none(),
            "a buffered line must not resolve"
        );

        // A durability proof publishes every waiting mapping, in line order.
        normalizer.mappings_committed();
        let resolved = state
            .lock()
            .unwrap()
            .resolve(&position)
            .expect("a proven line resolves");
        assert_eq!(resolved.line(), 1);
        assert_eq!(resolved.byte_offset(), 0);

        // Ordering and the capacity bound are preserved by the move.
        normalizer.feed(b"two\n");
        let _ = normalizer.next_pending();
        normalizer.mappings_committed();
        let guard = state.lock().unwrap();
        assert_eq!(
            guard.resolve(&TailPosition::new(TailId::new("tail"), 5, 0)),
            None,
            "a later revision belongs to the second line, not the first"
        );
        assert_eq!(
            guard
                .resolve(&TailPosition::new(TailId::new("tail"), 7, 0))
                .map(|position| position.line()),
            Some(2)
        );
    }

    #[test]
    fn a_dropped_batch_expires_its_mappings() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        normalizer.feed(b"gone\n");
        let item = normalizer.next_pending().expect("a finalized line");
        assert!(matches!(item, PendingLine::Line { line: 1, .. }));
        let position = TailPosition::new(TailId::new("tail"), 3, 0);

        // The batch was durably dropped as an explicit gap: the line does
        // not exist, so the mapping must never resolve.
        normalizer.mappings_dropped();
        assert!(state.lock().unwrap().resolve(&position).is_none());

        // Even a later durability proof cannot resurrect it.
        normalizer.mappings_committed();
        assert!(state.lock().unwrap().resolve(&position).is_none());
    }

    #[test]
    fn the_mapping_capacity_keeps_the_newest_lines() {
        let state = state();
        let mut normalizer = Normalizer::new(Arc::clone(&state));
        let mut revisions = Vec::new();
        for _ in 0..(TAIL_MAPPING_CAPACITY + 5) {
            normalizer.feed(b"l");
            revisions.push(state.lock().unwrap().snapshot().position().revision());
            normalizer.feed(b"\n");
            while normalizer.next_pending().is_some() {}
        }
        normalizer.mappings_committed();

        let guard = state.lock().unwrap();
        // The oldest mapping fell out of the bounded window and expires; the
        // newest still resolves to its own line, so ordering was preserved.
        assert!(
            guard
                .resolve(&TailPosition::new(TailId::new("tail"), revisions[0], 0))
                .is_none(),
            "the oldest mapping is evicted by the capacity bound"
        );
        assert_eq!(
            guard
                .resolve(&TailPosition::new(
                    TailId::new("tail"),
                    *revisions.last().expect("a last revision"),
                    0
                ))
                .map(|position| position.line()),
            Some(TAIL_MAPPING_CAPACITY as u64 + 5)
        );
    }
}
