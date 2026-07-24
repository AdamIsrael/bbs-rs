-- Attachments (#95): link an existing file-area file to a board post or a
-- private mail message.
--
-- A join, deliberately, rather than a copy of the bytes. The file keeps living
-- in its area, so it stays browsable in the TUI, fetchable over SFTP, and —
-- the part that matters — still governed by that area's `min_read_role`.
-- Attaching never widens access: the reader re-checks the ACL every time it
-- lists attachments, so a file from a restricted area is simply invisible to a
-- viewer who couldn't already read it.
--
-- CASCADE on both sides: deleting the post/mail, or the underlying file, drops
-- the link rather than leaving a row pointing at nothing.
CREATE TABLE message_attachments (
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    file_id    INTEGER NOT NULL REFERENCES files(id)    ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (message_id, file_id)
);

CREATE TABLE mail_attachments (
    mail_id    INTEGER NOT NULL REFERENCES mail(id)  ON DELETE CASCADE,
    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (mail_id, file_id)
);

-- The primary keys already index the (item -> files) direction the reader uses;
-- these cover the reverse, for "what still references this file" on delete.
CREATE INDEX idx_message_attachments_file ON message_attachments (file_id);
CREATE INDEX idx_mail_attachments_file ON mail_attachments (file_id);
