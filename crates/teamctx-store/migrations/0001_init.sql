-- teamctx schema, version 1. Plan section 6 is the design; this file is the DDL.
-- Times are UTC Unix milliseconds. Booleans are INTEGER 0/1 with a CHECK.
-- External ids (Discord snowflakes) are stored as TEXT.

CREATE TABLE sources (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL CHECK (kind = 'discord'),
    external_id  TEXT    NOT NULL,                  -- guild id
    name         TEXT    NOT NULL,
    UNIQUE (kind, external_id)
);

CREATE TABLE channels (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id           INTEGER NOT NULL REFERENCES sources (id),
    external_id         TEXT    NOT NULL,
    -- Threads and forum posts point at their parent. Deleting a parent removes the subtree.
    parent_id           INTEGER REFERENCES channels (id) ON DELETE CASCADE,
    kind                TEXT    NOT NULL
        CHECK (kind IN ('text', 'thread', 'forum', 'forum_post', 'announcement')),
    name                TEXT    NOT NULL,
    topic               TEXT,
    -- Effective visibility: the connector's decision AND every ancestor's. Only
    -- messages in channels with is_included = 1 are readable or searchable.
    is_included         INTEGER NOT NULL CHECK (is_included IN (0, 1)),
    archived            INTEGER NOT NULL CHECK (archived IN (0, 1)),
    -- What the connector asked for, kept apart from inherited visibility so that
    -- re-including a parent restores exactly the descendants that were requested.
    requested_included  INTEGER NOT NULL DEFAULT 1 CHECK (requested_included IN (0, 1)),
    UNIQUE (source_id, external_id)
);

CREATE TABLE members (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id     INTEGER NOT NULL REFERENCES sources (id),
    external_id   TEXT    NOT NULL,
    display_name  TEXT    NOT NULL,
    is_bot        INTEGER NOT NULL CHECK (is_bot IN (0, 1)),
    UNIQUE (source_id, external_id)
);

CREATE TABLE messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id        INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    external_id       TEXT    NOT NULL,             -- snowflake
    author_id         INTEGER REFERENCES members (id),
    content           TEXT    NOT NULL,             -- normalised text
    reply_to_ext      TEXT,                         -- external id of the referenced message
    has_attachments   INTEGER NOT NULL CHECK (has_attachments IN (0, 1)),
    attachment_names  TEXT    NOT NULL,             -- JSON array of names, never file contents
    created_at        INTEGER NOT NULL,
    edited_at         INTEGER,
    UNIQUE (channel_id, external_id)
);
CREATE INDEX messages_channel_time ON messages (channel_id, created_at);

-- Mentioned user ids are kept apart from the normalised message text.
CREATE TABLE message_mentions (
    message_id        INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    external_user_id  TEXT    NOT NULL,
    PRIMARY KEY (message_id, external_user_id)
);
CREATE INDEX mentions_user ON message_mentions (external_user_id, message_id);

-- External-content FTS5 index over messages.content, kept in sync by the triggers
-- below. FTS5 cannot delete by value alone: the 'delete' command needs the old text.
CREATE VIRTUAL TABLE messages_fts USING fts5 (
    content,
    content = 'messages',
    content_rowid = 'id',
    tokenize = 'porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
END;

-- An edit replaces the indexed text: delete the old entry, then insert the new one.
CREATE TRIGGER messages_au AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;

-- One cursor per channel and per thread.
CREATE TABLE sync_cursors (
    channel_id             INTEGER PRIMARY KEY REFERENCES channels (id) ON DELETE CASCADE,
    newest_seen_ext        TEXT,                    -- catch-up: messages after this id
    oldest_backfilled_ext  TEXT,                    -- backfill: messages before this id
    backfill_done          INTEGER NOT NULL CHECK (backfill_done IN (0, 1)),
    updated_at             INTEGER NOT NULL
);

CREATE TABLE record_entries (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,  -- short ids D-12, T-7, Q-3
    kind                    TEXT    NOT NULL CHECK (kind IN ('decision', 'task', 'question')),
    status                  TEXT    NOT NULL,       -- allowed values depend on kind, see below
    title                   TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    title_is_copied         INTEGER NOT NULL CHECK (title_is_copied IN (0, 1)),
    owner_member_id         INTEGER REFERENCES members (id),
    due_at                  INTEGER,                -- tasks only
    source_message_id       INTEGER REFERENCES messages (id) ON DELETE SET NULL,
    source_deleted          INTEGER NOT NULL DEFAULT 0 CHECK (source_deleted IN (0, 1)),
    superseded_by           INTEGER REFERENCES record_entries (id),
    answered_by_message_id  INTEGER REFERENCES messages (id) ON DELETE SET NULL,
    created_by_member_id    INTEGER NOT NULL REFERENCES members (id),
    created_via             TEXT    NOT NULL
        CHECK (created_via IN ('context_menu', 'slash', 'reaction', 'web', 'cli')),
    tags                    TEXT    NOT NULL,       -- space-separated, lowercase
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL CHECK (updated_at >= created_at),
    -- Each kind has its own status set.
    CHECK (
        (kind = 'decision' AND status IN ('active', 'superseded'))
        OR (kind = 'task' AND status IN ('open', 'done', 'dropped'))
        OR (kind = 'question' AND status IN ('open', 'answered', 'dropped'))
    ),
    CHECK (due_at IS NULL OR kind = 'task'),
    -- A decision is superseded exactly when it names its replacement.
    CHECK ((status = 'superseded') = (superseded_by IS NOT NULL)),
    CHECK (answered_by_message_id IS NULL OR status = 'answered'),
    -- Once the source message is gone the reference is cleared and the flag is set.
    CHECK (source_deleted = 0 OR source_message_id IS NULL)
);

CREATE VIRTUAL TABLE record_fts USING fts5 (
    title,
    tags,
    content = 'record_entries',
    content_rowid = 'id',
    tokenize = 'porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER record_ai AFTER INSERT ON record_entries BEGIN
    INSERT INTO record_fts (rowid, title, tags) VALUES (new.id, new.title, new.tags);
END;

CREATE TRIGGER record_ad AFTER DELETE ON record_entries BEGIN
    INSERT INTO record_fts (record_fts, rowid, title, tags)
    VALUES ('delete', old.id, old.title, old.tags);
END;

-- Only title and tags are indexed, so other updates leave the index alone.
CREATE TRIGGER record_au AFTER UPDATE OF title, tags ON record_entries BEGIN
    INSERT INTO record_fts (record_fts, rowid, title, tags)
    VALUES ('delete', old.id, old.title, old.tags);
    INSERT INTO record_fts (rowid, title, tags) VALUES (new.id, new.title, new.tags);
END;

-- Append-only audit log.
CREATE TABLE record_events (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id         INTEGER NOT NULL REFERENCES record_entries (id),
    actor_member_id  INTEGER REFERENCES members (id),  -- NULL for system actions
    action           TEXT    NOT NULL,
    detail           TEXT,
    at               INTEGER NOT NULL
);

CREATE TABLE access_tokens (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,          -- e.g. "alex-laptop"
    token_sha256  BLOB    NOT NULL UNIQUE CHECK (length(token_sha256) = 32),  -- never the token
    scopes        TEXT    NOT NULL DEFAULT 'read' CHECK (scopes = 'read'),
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER,
    revoked_at    INTEGER
);

CREATE TABLE llm_usage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    at             INTEGER NOT NULL,
    member_id      INTEGER REFERENCES members (id),
    provider       TEXT    NOT NULL,
    model          TEXT    NOT NULL,
    input_tokens   INTEGER NOT NULL,
    output_tokens  INTEGER NOT NULL,
    ok             INTEGER NOT NULL
);

-- Per-member state, for example the last /catchup.
CREATE TABLE user_state (
    member_id        INTEGER PRIMARY KEY REFERENCES members (id),
    last_catchup_at  INTEGER NOT NULL
);
