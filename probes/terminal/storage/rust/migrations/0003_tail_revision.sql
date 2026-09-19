-- Throwaway probe C Gate C addition. NOT production storage.
--
-- Revision-addressed tail mapping. The old tail_map design kept one row per
-- terminal that every append overwrote with the last chunk's start offset,
-- so no historical revision was ever actually resolvable. Instead every
-- committed line persists an immutable tail revision whose cursor is the
-- exact end-of-line byte position: a reader holding a revision can resolve
-- it to a fixed (line, byte_offset) cursor after rotation, and resource
-- recovery expires revisions transactionally when it deletes the holding
-- segment (an unresolvable revision fails explicitly, never fabricates a
-- position).
DROP TABLE tail_map;

CREATE TABLE tail_revision (
    terminal_id TEXT NOT NULL REFERENCES terminal (terminal_id),
    revision    INTEGER NOT NULL CHECK (revision >= 1),
    line        INTEGER NOT NULL CHECK (line >= 1),
    byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
    segment_id  INTEGER NOT NULL REFERENCES segment (segment_id),
    PRIMARY KEY (terminal_id, revision)
);

CREATE INDEX idx_tail_revision_segment ON tail_revision (segment_id);
