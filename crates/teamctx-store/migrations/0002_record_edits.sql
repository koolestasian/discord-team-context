-- Schema version 2: what the human-captured write path needs (plan sections 6 and 8).
-- Added as a new migration instead of editing 0001: databases created by 0001 exist
-- (the demo, test hosts), and `user_version` must move forward for them.

-- Retry-safe creation: the first request with a key creates the entry, later requests
-- with the same key return it. The key is the caller's interaction id, never user text.
CREATE TABLE record_requests (
    request_key  TEXT    PRIMARY KEY CHECK (length(request_key) BETWEEN 1 AND 128),
    entry_id     INTEGER NOT NULL REFERENCES record_entries (id),
    created_at   INTEGER NOT NULL
);

-- The audit log is read per entry (entry history, export log).
CREATE INDEX record_events_entry ON record_events (entry_id, at, id);

-- Append-only audit log (plan section 6): rows can be added, never changed or removed.
CREATE TRIGGER record_events_no_update BEFORE UPDATE ON record_events BEGIN
    SELECT RAISE(ABORT, 'record_events is append-only');
END;

CREATE TRIGGER record_events_no_delete BEFORE DELETE ON record_events BEGIN
    SELECT RAISE(ABORT, 'record_events is append-only');
END;
