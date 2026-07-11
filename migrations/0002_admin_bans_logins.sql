-- Access control: per-user bans, IP bans, and a login-attempt audit trail.
-- The 'admin' role is stored in the existing free-form users.role column.

ALTER TABLE users ADD COLUMN banned_at INTEGER;   -- NULL = not banned

CREATE TABLE ip_bans (
    ip         TEXT PRIMARY KEY,
    reason     TEXT    NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE logins (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    username   TEXT    NOT NULL,   -- as attempted (may not be a real user)
    ip         TEXT,               -- NULL if unavailable
    success    INTEGER NOT NULL,   -- 0 = rejected, 1 = accepted
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_logins_username ON logins (username, id);
CREATE INDEX idx_logins_ip ON logins (ip, id);
