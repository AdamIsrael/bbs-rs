-- Board moderation & ACLs: per-board read/write role requirements and a lock
-- flag, plus a pin flag on individual messages.
--
-- Roles are ordered guest < user < admin. Defaults preserve today's behavior:
-- anyone (incl. guests) may read; registered users may post; boards unlocked.

ALTER TABLE boards ADD COLUMN min_read_role  TEXT    NOT NULL DEFAULT 'guest';
ALTER TABLE boards ADD COLUMN min_write_role TEXT    NOT NULL DEFAULT 'user';
ALTER TABLE boards ADD COLUMN locked         INTEGER NOT NULL DEFAULT 0;

ALTER TABLE messages ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;

-- The seeded Announcements board becomes admin-only to post to (still readable
-- by everyone) — a demonstration of the write ACL. No-op if it was renamed.
UPDATE boards SET min_write_role = 'admin' WHERE name = 'Announcements';
