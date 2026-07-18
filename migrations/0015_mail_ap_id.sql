-- Inbound remote DMs land in `mail` (epic #113, phase #110 Slice B).
--
-- A direct `Note` addressed to a local user is stored as a mail row from the
-- remote actor's shadow row. Give mail an `ap_id` so a redelivery (the same DM
-- arriving twice) is idempotent — same key, one row. NULL for local mail, and
-- SQLite treats NULLs as distinct, so existing rows are unaffected.
--
-- `mail` is not under any FTS index, so this is a plain additive column.
ALTER TABLE mail ADD COLUMN ap_id TEXT;
CREATE UNIQUE INDEX idx_mail_ap_id ON mail (ap_id);
