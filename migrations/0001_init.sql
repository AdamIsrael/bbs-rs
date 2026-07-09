-- Initial BBS schema: users, boards, board messages, and private mail.
-- Timestamps are Unix seconds (INTEGER) filled in by the application.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL UNIQUE,
    password_hash TEXT    NOT NULL,
    role          TEXT    NOT NULL DEFAULT 'user',   -- 'guest' | 'user'
    created_at    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE boards (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT ''
);

CREATE TABLE messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    board_id   INTEGER NOT NULL REFERENCES boards(id),
    author_id  INTEGER NOT NULL REFERENCES users(id),
    subject    TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE mail (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id    INTEGER NOT NULL REFERENCES users(id),
    to_id      INTEGER NOT NULL REFERENCES users(id),
    subject    TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0,
    read_at    INTEGER
);

CREATE INDEX idx_messages_board ON messages (board_id, id);
CREATE INDEX idx_mail_to ON mail (to_id, id);
