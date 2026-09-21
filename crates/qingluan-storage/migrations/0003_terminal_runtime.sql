-- S3 terminal runtime registry schema.
--
-- One row per terminal records the durable lifecycle needed to recover
-- after a daemon restart: the terminal's phase, its independent process
-- and output dimensions, whether a stop was committed, its window size,
-- and a monotonic revision bumped by every change. It deliberately holds
-- no PID, no cgroup path, no environment snapshot, and no lifecycle
-- event: correlation with OS resources is by the terminal identity
-- components only, so a recovered row can never make recovery signal a
-- stale process.
--
-- Invariants pinned here:
--   * `phase` is one of `starting` (slot reserved, process not confirmed),
--     `running` (root process confirmed started), `cleaning` (stop
--     committed or an unfinished record found after a restart, resources
--     not yet reclaimed), `released` (slot free; the terminal no longer
--     occupies quota). `starting/running/cleaning` all occupy; only
--     `released` does not.
--   * the process and output dimensions are independent:
--     `process_state` in {running, exited, interrupted} with a known exit
--     kind/value required only when `exited`; `output_state` in
--     {open, closed} with a close reason required only when `closed`.
--     Neither dimension constrains the other, so "process exited, output
--     still open" and "output closed, process still running" are both
--     representable.
--   * an unknown outcome is `interrupted`, never a sentinel exit code or
--     a fabricated exit; recovery only ever sets it.
--   * `stopping` is a separate latch (a stop flow has been committed);
--     `revision` starts at 1 and is monotonically bumped by every change.
--   * `size_rows`/`size_columns` are the last known window size (non-zero).

CREATE TABLE terminal_runtime (
    session_source      TEXT NOT NULL,
    external_session_id TEXT NOT NULL,
    terminal_id         TEXT NOT NULL,
    phase               TEXT NOT NULL
                        CHECK (phase IN ('starting', 'running', 'cleaning', 'released')),
    process_state       TEXT NOT NULL
                        CHECK (process_state IN ('running', 'exited', 'interrupted')),
    exit_kind           TEXT CHECK (exit_kind IN ('code', 'signal')),
    exit_value          INTEGER,
    output_state        TEXT NOT NULL CHECK (output_state IN ('open', 'closed')),
    output_end          TEXT CHECK (output_end IN ('eof', 'forced', 'read_error', 'interrupted')),
    stopping            INTEGER NOT NULL DEFAULT 0 CHECK (stopping IN (0, 1)),
    size_rows           INTEGER NOT NULL CHECK (size_rows > 0),
    size_columns        INTEGER NOT NULL CHECK (size_columns > 0),
    revision            INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_ms          INTEGER NOT NULL,
    updated_ms          INTEGER NOT NULL,
    PRIMARY KEY (session_source, external_session_id, terminal_id),
    CHECK ((process_state = 'exited') = (exit_kind IS NOT NULL)),
    CHECK ((process_state = 'exited') = (exit_value IS NOT NULL)),
    CHECK ((output_state = 'closed') = (output_end IS NOT NULL))
);

UPDATE meta SET value = '3' WHERE key = 'format_version';
UPDATE meta SET value = '3' WHERE key = 'schema_generation';
