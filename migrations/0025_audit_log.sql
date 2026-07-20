-- Moderation / audit log (#74): who did what, for operator accountability.
--
-- One row per moderator action (ban, unban, role change, board lock, pin,
-- post delete, broadcast, auto-ban). `actor` is the acting user's name, the
-- literal 'bbsctl' for out-of-process CLI actions, or 'system' for automated
-- ones (auto-ban). `target` is what was acted on (a username, IP, board, or
-- post subject); `detail` carries optional extra context (a new role, a ban
-- reason, the broadcast text). Append-only — nothing updates or deletes rows.
CREATE TABLE audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    actor      TEXT    NOT NULL,
    action     TEXT    NOT NULL,
    target     TEXT    NOT NULL,
    detail     TEXT
);
