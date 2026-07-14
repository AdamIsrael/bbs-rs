-- Full-text search over board messages via an FTS5 external-content index.
-- The index stores only the tokenized subject/body; the rows live in
-- `messages` (content='messages', content_rowid='id'), and triggers keep the
-- index in sync on insert/update/delete.

CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject,
    body,
    content='messages',
    content_rowid='id'
);

-- Backfill any existing messages.
INSERT INTO messages_fts(rowid, subject, body)
    SELECT id, subject, body FROM messages;

-- Keep the index in sync. Deletes/updates use the special 'delete' command
-- required for external-content tables.
CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, body)
        VALUES (new.id, new.subject, new.body);
END;

CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, body)
        VALUES ('delete', old.id, old.subject, old.body);
END;

CREATE TRIGGER messages_fts_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, body)
        VALUES ('delete', old.id, old.subject, old.body);
    INSERT INTO messages_fts(rowid, subject, body)
        VALUES (new.id, new.subject, new.body);
END;
