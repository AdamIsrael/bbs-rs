-- Full-text search over private mail (#93), mirroring `messages_fts` (0012):
-- an FTS5 external-content index over the mail subject/body, kept in sync by
-- insert/update/delete triggers. The rows live in `mail`; search is always
-- scoped to the requesting user's own mailbox (`to_id`) in the query itself.
CREATE VIRTUAL TABLE mail_fts USING fts5(
    subject,
    body,
    content='mail',
    content_rowid='id'
);

-- Backfill existing mail.
INSERT INTO mail_fts(rowid, subject, body)
    SELECT id, subject, body FROM mail;

CREATE TRIGGER mail_fts_ai AFTER INSERT ON mail BEGIN
    INSERT INTO mail_fts(rowid, subject, body)
        VALUES (new.id, new.subject, new.body);
END;

CREATE TRIGGER mail_fts_ad AFTER DELETE ON mail BEGIN
    INSERT INTO mail_fts(mail_fts, rowid, subject, body)
        VALUES ('delete', old.id, old.subject, old.body);
END;

CREATE TRIGGER mail_fts_au AFTER UPDATE ON mail BEGIN
    INSERT INTO mail_fts(mail_fts, rowid, subject, body)
        VALUES ('delete', old.id, old.subject, old.body);
    INSERT INTO mail_fts(rowid, subject, body)
        VALUES (new.id, new.subject, new.body);
END;
