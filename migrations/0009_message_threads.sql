-- Message threading: a reply points at its parent message via `parent_id`.
-- Top-level posts have NULL parent. Replies to a since-deleted parent are
-- treated as roots by the reader (the column is not enforced ON DELETE).

ALTER TABLE messages ADD COLUMN parent_id INTEGER REFERENCES messages(id);

CREATE INDEX idx_messages_parent ON messages (parent_id);
