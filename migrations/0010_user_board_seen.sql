-- Per-user "last seen" watermark for each board, powering unread / "new since
-- last call" highlighting. `last_seen_at` is a Unix timestamp; a message with a
-- newer `created_at` is unread for that user. Rows are created lazily the first
-- time a user opens a board.

CREATE TABLE user_board_seen (
    user_id      INTEGER NOT NULL REFERENCES users(id),
    board_id     INTEGER NOT NULL REFERENCES boards(id),
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, board_id)
);
