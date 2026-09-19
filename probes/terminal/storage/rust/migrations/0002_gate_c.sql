-- Throwaway probe C Gate C addition. NOT production storage.
--
-- refuse_new_start latches when the bounded drain overflowed while the
-- writer-side I/O was failing: the terminal recorded an explicit missing
-- range and must refuse a NEW writer start until an operator runs a
-- destructive rebuild (which clears the latch in the same transaction).
ALTER TABLE terminal ADD COLUMN refuse_new_start INTEGER NOT NULL DEFAULT 0
    CHECK (refuse_new_start IN (0, 1));
