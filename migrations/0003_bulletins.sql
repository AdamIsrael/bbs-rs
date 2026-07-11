-- Sysop bulletins: short dated announcements shown after login, beyond the
-- single-line MOTD (bbs.welcome). Authored by operators via bbsctl.

CREATE TABLE bulletins (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);
