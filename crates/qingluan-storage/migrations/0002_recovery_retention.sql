-- S2 recovery and retention schema: stream-scoped gaps and the raw
-- retained floor.
--
-- `log_gap` is recreated stream-scoped: `kind` separates normalized
-- line-range gaps from raw byte-range gaps (ranges never merge across
-- kinds), and the exclusive range lives in `range_start`/`range_end`
-- (1-based line numbers for `normalized`, stream byte offsets for
-- `raw`). The append path never wrote the old shape, so the conversion
-- only preserves hand-seeded rows. `terminal` gains
-- `retained_first_offset`, the raw-stream mirror of
-- `retained_first_line`: reclamation advances each floor to the first
-- stream position still backed by a surviving segment row; the floor is
-- expiry, never a `log_gap`.

CREATE TABLE log_gap_v2 (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id         TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('raw', 'normalized')),
    range_start         INTEGER NOT NULL,
    range_end           INTEGER NOT NULL,
    reason              TEXT NOT NULL CHECK (reason IN ('missing', 'truncated', 'corrupt')),
    created_ms          INTEGER NOT NULL,
    PRIMARY KEY (session_source, external_session_id, terminal_id, kind, range_start),
    FOREIGN KEY (session_source, external_session_id, terminal_id)
        REFERENCES terminal (session_source, external_session_id, terminal_id),
    CHECK (range_end > range_start),
    CHECK (
        (kind = 'normalized' AND range_start >= 1)
        OR (kind = 'raw' AND range_start >= 0)
    )
);

INSERT INTO log_gap_v2
    (session_source, external_session_id, terminal_id, kind,
     range_start, range_end, reason, created_ms)
SELECT session_source, external_session_id, terminal_id, 'normalized',
       first_line, last_line, reason, created_ms
  FROM log_gap;

DROP TABLE log_gap;
ALTER TABLE log_gap_v2 RENAME TO log_gap;

ALTER TABLE terminal ADD COLUMN retained_first_offset INTEGER NOT NULL DEFAULT 0
    CHECK (retained_first_offset >= 0);

UPDATE meta SET value = '2' WHERE key = 'format_version';
UPDATE meta SET value = '2' WHERE key = 'schema_generation';
