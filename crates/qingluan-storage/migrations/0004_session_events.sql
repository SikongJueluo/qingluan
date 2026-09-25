-- S5 durable session lifecycle events and their acknowledgement watermarks.
--
-- A session (keyed by `session_source` + `external_session_id`, the same
-- composite identity used by every other table) owns one append-only
-- event stream. Each event is self-explanatory: it carries the exact
-- `terminal_id` it is about plus the complete typed payload
-- (`kind`/`exit_kind`/`exit_value` or `output_end`), so replay never has
-- to re-read a terminal record that may already be deleted. Event rows
-- deliberately store no output text, no environment, and no input.
--
-- `session_state` holds the three cumulative watermarks of one session:
--   * `last_committed_seq` — the highest sequence ever durably committed.
--     The next sequence is allocated **only** from this persistent
--     counter, never from `MAX(event_seq)` of retained rows: after a full
--     prune the event table is empty, so a MAX allocator would restart at
--     1 and reuse an already-published sequence.
--   * `acked_through_seq` — the cumulative ack bound (monotonic; repeats
--     are harmless and it never regresses).
--   * `pruned_through_seq` — the contiguous cleared prefix (`DELETE ...
--     WHERE event_seq <= through_seq`); advanced in the same transaction
--     as the delete.
-- Invariant, pinned by CHECK: `pruned_through_seq <= acked_through_seq
-- <= last_committed_seq`, all starting at zero.
--
-- Events intentionally have **no** foreign key to `terminal` or
-- `terminal_runtime`: protocol requires event replay to keep working
-- after the terminal record is deleted, so no delete may cascade into
-- `session_event`. Session events never auto-expire; only the explicit
-- prune path above clears them.

CREATE TABLE session_event (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id         TEXT NOT NULL,
    event_seq           INTEGER NOT NULL CHECK (event_seq >= 1),
    kind                TEXT NOT NULL CHECK (kind IN ('exited', 'output_closed')),
    exit_kind           TEXT CHECK (exit_kind IN ('code', 'signal')),
    exit_value          INTEGER,
    output_end          TEXT CHECK (output_end IN ('eof', 'forced', 'read_error', 'interrupted')),
    created_ms          INTEGER NOT NULL,
    PRIMARY KEY (session_source, external_session_id, event_seq),
    CHECK ((kind = 'exited') = (exit_kind IS NOT NULL)),
    CHECK ((kind = 'exited') = (exit_value IS NOT NULL)),
    CHECK ((kind = 'output_closed') = (output_end IS NOT NULL))
);

CREATE TABLE session_state (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    pruned_through_seq  INTEGER NOT NULL DEFAULT 0 CHECK (pruned_through_seq >= 0),
    acked_through_seq   INTEGER NOT NULL DEFAULT 0 CHECK (acked_through_seq >= 0),
    last_committed_seq  INTEGER NOT NULL DEFAULT 0 CHECK (last_committed_seq >= 0),
    PRIMARY KEY (session_source, external_session_id),
    CHECK (pruned_through_seq <= acked_through_seq),
    CHECK (acked_through_seq <= last_committed_seq)
);

UPDATE meta SET value = '4' WHERE key = 'format_version';
UPDATE meta SET value = '4' WHERE key = 'schema_generation';
