-- Polls / voting booth (#72): a poll has a question and two or more options;
-- each user casts at most one vote per poll (changeable while the poll is open).
-- Closing a poll (closed_at set) freezes voting but keeps results visible.
CREATE TABLE polls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    author_id  INTEGER NOT NULL REFERENCES users(id),
    question   TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    closed_at  INTEGER
);

CREATE TABLE poll_options (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    poll_id  INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    label    TEXT    NOT NULL
);

-- One vote per user per poll; option_id records the choice, updated on re-vote.
CREATE TABLE poll_votes (
    poll_id    INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    option_id  INTEGER NOT NULL REFERENCES poll_options(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (poll_id, user_id)
);

CREATE INDEX idx_poll_options_poll ON poll_options(poll_id, position);
CREATE INDEX idx_poll_votes_option ON poll_votes(option_id);
