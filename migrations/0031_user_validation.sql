-- New-user validation queue (#73): a nullable activation timestamp. NULL means
-- the account is pending sysop approval and can't log in; a set value means it's
-- active. New registrations start NULL only when [accounts] require_validation
-- is on; otherwise they're validated immediately.
ALTER TABLE users ADD COLUMN validated_at INTEGER;

-- Every account that already exists is active — only *new* registrations can be
-- gated, so nobody is locked out by this upgrade.
UPDATE users SET validated_at = created_at WHERE validated_at IS NULL;
