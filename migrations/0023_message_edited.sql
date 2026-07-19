-- Authors can edit their own posts (#92); record when, so the UI can mark it.
--
-- Nullable: NULL means "never edited", which is every existing row and every
-- fresh post. A post that's been edited carries the Unix time of its last edit.
-- Purely informational — nothing keys off it — so an additive column is all
-- that's needed, and the messages_fts triggers keep search in sync with the
-- edited subject/body on their own (they fire on the UPDATE).
ALTER TABLE messages ADD COLUMN edited_at INTEGER;
