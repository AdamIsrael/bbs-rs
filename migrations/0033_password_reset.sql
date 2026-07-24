-- Account recovery (#76): a sysop can reset a password with `bbsctl passwd`,
-- which hands out a one-time temporary password and flags the account so the
-- next login is forced through the change-password screen before anything else.
--
-- The flag is the *time* of the reset rather than a bare boolean, so the sweeper
-- can tell a stale session (started before the reset — e.g. the intruder you are
-- resetting because of) from the session that just logged in and is sitting on
-- the change-password gate. NULL means no pending reset, which is where every
-- existing account starts.
ALTER TABLE users ADD COLUMN password_reset_at INTEGER;
