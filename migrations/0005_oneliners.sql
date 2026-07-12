-- Oneliners: a shared "graffiti wall" of short one-line public posts that any
-- registered user can append to. Read-only for guests, like boards and mail.

CREATE TABLE oneliners (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    author_id  INTEGER NOT NULL REFERENCES users(id),
    body       TEXT    NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_oneliners_id ON oneliners (id);
