-- Throwaway probe C schema (foundation only). NOT production storage.
--
-- Invariants this schema is meant to pin down (see docs/design
-- terminal-technical-validation-plan.md §6):
--   * terminal.log_epoch only changes on destructive rebuild;
--   * terminal.line_watermark never decreases and line numbers are never reused;
--   * segment rows carry the available line range plus committed/fsynced bytes;
--   * log_gap records explicit missing line ranges (never fabricated continuity);
--   * session events use a composite (session_id, event_seq) key and obey
--     pruned <= acked <= last_appended;
--   * tail_map locates the current tail line across rotation.

CREATE TABLE terminal (
    terminal_id    TEXT PRIMARY KEY,
    terminal_uuid  BLOB NOT NULL,
    log_epoch      BLOB NOT NULL,
    line_watermark INTEGER NOT NULL DEFAULT 0 CHECK (line_watermark >= 0),
    active_segment INTEGER,
    degraded       INTEGER NOT NULL DEFAULT 0 CHECK (degraded IN (0, 1)),
    process_status TEXT NOT NULL DEFAULT 'running'
                   CHECK (process_status IN ('running', 'exited', 'unknown')),
    output_status  TEXT NOT NULL DEFAULT 'open'
                   CHECK (output_status IN ('open', 'closed')),
    exit_code      INTEGER,
    exit_kind      TEXT CHECK (exit_kind IS NULL OR exit_kind IN ('clean', 'signal', 'unknown'))
);

CREATE TABLE segment (
    segment_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id     TEXT NOT NULL REFERENCES terminal (terminal_id),
    file_name       TEXT NOT NULL UNIQUE,
    first_line      INTEGER NOT NULL CHECK (first_line >= 1),
    last_line       INTEGER NOT NULL,
    committed_bytes INTEGER NOT NULL DEFAULT 0 CHECK (committed_bytes >= 0),
    fsynced_bytes   INTEGER NOT NULL DEFAULT 0 CHECK (fsynced_bytes >= 0),
    state           TEXT NOT NULL DEFAULT 'active'
                    CHECK (state IN ('active', 'sealed', 'quarantined', 'missing')),
    CHECK (last_line >= first_line),
    CHECK (fsynced_bytes <= committed_bytes)
);

CREATE INDEX idx_segment_terminal ON segment (terminal_id);

CREATE TABLE log_gap (
    terminal_id TEXT NOT NULL REFERENCES terminal (terminal_id),
    first_line  INTEGER NOT NULL CHECK (first_line >= 1),
    last_line   INTEGER NOT NULL,
    reason      TEXT NOT NULL CHECK (reason IN ('missing', 'truncated', 'corrupt')),
    created_ms  INTEGER NOT NULL,
    PRIMARY KEY (terminal_id, first_line),
    CHECK (last_line > first_line)
);

CREATE TABLE session (
    session_id TEXT PRIMARY KEY
);

-- Composite session event key: events are addressed per session by sequence.
CREATE TABLE session_event (
    session_id TEXT NOT NULL REFERENCES session (session_id),
    event_seq  INTEGER NOT NULL CHECK (event_seq >= 1),
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    PRIMARY KEY (session_id, event_seq)
);

CREATE TABLE session_state (
    session_id         TEXT PRIMARY KEY REFERENCES session (session_id),
    pruned_through_seq INTEGER NOT NULL DEFAULT 0,
    acked_through_seq  INTEGER NOT NULL DEFAULT 0,
    last_appended_seq  INTEGER NOT NULL DEFAULT 0,
    CHECK (pruned_through_seq <= acked_through_seq),
    CHECK (acked_through_seq <= last_appended_seq)
);

-- Tail mapping: the current (in-progress) tail line and the byte offset at
-- which the holding segment begins, so a (line, offset) cursor can be
-- resolved to a segment after rotation.
CREATE TABLE tail_map (
    terminal_id TEXT PRIMARY KEY REFERENCES terminal (terminal_id),
    line        INTEGER NOT NULL CHECK (line >= 1),
    byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
    segment_id  INTEGER NOT NULL REFERENCES segment (segment_id)
);
