-- Temporary IP bans: auto-bans (from repeated failed logins) expire, while
-- manual bans stay permanent. NULL = permanent.

ALTER TABLE ip_bans ADD COLUMN expires_at INTEGER;
