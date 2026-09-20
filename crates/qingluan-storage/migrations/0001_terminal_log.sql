-- S2 terminal log storage schema (foundation).
--
-- Identity is the composite key of `LogIdentity`: (session_source,
-- external_session_id, terminal_id) match core `TerminalRef`. `terminal_id`
-- and `log_epoch` are qingluan-minted canonical UUID text at this seam
-- (hyphenated lowercase: exactly one textual spelling per 16-byte
-- identity); the exact text of `terminal_id` is a key column while the
-- UUID bytes of both identities live in `terminal_uuid`/`log_epoch` and
-- must equal the segment headers.
--
-- Two independent streams share one durable format: `normalized` (UTF-8
-- line text, 1-based `(line, byte_offset)` positions) and `raw`
-- (arbitrary output bytes kept for archival, stream byte offsets). The
-- `log_epoch` is shared by both streams of one terminal; segments,
-- sequences, and watermarks never cross streams.
--
-- Invariants pinned here:
--   * `terminal.log_epoch` changes only on a destructive rebuild;
--   * `line_watermark` (normalized) and `raw_watermark` (raw) are
--     per-stream, never decrease, and their numbers are never reused;
--   * `retained_first_line` is the floor of the still-retained line range
--     (advances when old segments are reclaimed; reader expiry derives
--     from it, not from surviving segment rows);
--   * every segment row carries exactly one stream `kind`: normalized
--     rows own the exclusive line range `first_line`/`last_line`, raw
--     rows own the exclusive raw byte range `first_offset`/`last_offset`
--     in stream coordinates; ranges never mix kinds;
--   * `segment` rows carry committed/fsynced bytes with
--     `fsynced_bytes <= committed_bytes`;
--   * `log_gap` records explicit missing ranges, never fabricated
--     continuity; it is reserved for the recovery pass (S2b) and is not
--     written by the append path.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE terminal (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id         TEXT NOT NULL,
    terminal_uuid       BLOB NOT NULL CHECK (length(terminal_uuid) = 16),
    log_epoch           BLOB NOT NULL CHECK (length(log_epoch) = 16),
    line_watermark      INTEGER NOT NULL DEFAULT 0 CHECK (line_watermark >= 0),
    raw_watermark       INTEGER NOT NULL DEFAULT 0 CHECK (raw_watermark >= 0),
    retained_first_line INTEGER NOT NULL DEFAULT 1 CHECK (retained_first_line >= 1),
    active_normalized_segment INTEGER REFERENCES segment (segment_id),
    active_raw_segment  INTEGER REFERENCES segment (segment_id),
    degraded            INTEGER NOT NULL DEFAULT 0 CHECK (degraded IN (0, 1)),
    refuse_new_start    INTEGER NOT NULL DEFAULT 0 CHECK (refuse_new_start IN (0, 1)),
    PRIMARY KEY (session_source, external_session_id, terminal_id)
);

CREATE TABLE segment (
    segment_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    session_source  TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id     TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('raw', 'normalized')),
    file_name       TEXT NOT NULL UNIQUE,
    first_line      INTEGER,
    last_line       INTEGER,
    first_offset    INTEGER NOT NULL DEFAULT 0 CHECK (first_offset >= 0),
    last_offset     INTEGER NOT NULL DEFAULT 0 CHECK (last_offset >= first_offset),
    committed_bytes INTEGER NOT NULL DEFAULT 0 CHECK (committed_bytes >= 0),
    fsynced_bytes   INTEGER NOT NULL DEFAULT 0 CHECK (fsynced_bytes >= 0),
    state           TEXT NOT NULL DEFAULT 'active'
                    CHECK (state IN ('active', 'sealed', 'quarantined', 'missing')),
    created_ms      INTEGER NOT NULL,
    sealed_ms       INTEGER,
    FOREIGN KEY (session_source, external_session_id, terminal_id)
        REFERENCES terminal (session_source, external_session_id, terminal_id),
    CHECK (last_line >= first_line),
    CHECK (
        (kind = 'normalized' AND first_line >= 1 AND last_line >= first_line)
        OR (kind = 'raw' AND first_line IS NULL AND last_line IS NULL)
    ),
    CHECK (fsynced_bytes <= committed_bytes)
);

CREATE INDEX idx_segment_terminal
    ON segment (session_source, external_session_id, terminal_id, kind, segment_id);

CREATE TABLE log_gap (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id         TEXT NOT NULL,
    first_line          INTEGER NOT NULL CHECK (first_line >= 1),
    last_line           INTEGER NOT NULL,
    reason              TEXT NOT NULL CHECK (reason IN ('missing', 'truncated', 'corrupt')),
    created_ms          INTEGER NOT NULL,
    PRIMARY KEY (session_source, external_session_id, terminal_id, first_line),
    FOREIGN KEY (session_source, external_session_id, terminal_id)
        REFERENCES terminal (session_source, external_session_id, terminal_id),
    CHECK (last_line > first_line)
);

INSERT INTO meta (key, value) VALUES
    ('format_version', '1'),
    ('schema_generation', '1'),
    ('min_reader_version', '1');
