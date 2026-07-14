-- User profiles: optional self-description fields shown on a profile screen and, for
-- the signature, appended beneath the author's board posts. All default to the
-- empty string so existing accounts need no backfill.

ALTER TABLE users ADD COLUMN real_name TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN location  TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN tagline   TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN signature TEXT NOT NULL DEFAULT '';
