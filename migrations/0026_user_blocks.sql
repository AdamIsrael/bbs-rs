-- Per-user ignore / block list (#97): a user hides a blocked user's board
-- posts and refuses their mail and pages.
--
-- (blocker, blocked) is unique — blocking twice is a no-op. Both columns are
-- local user ids; a self-block is prevented by the caller, and blocking an
-- admin is refused (operators must always be able to reach users). Rows are
-- removed on unblock; nothing else keys off them.
CREATE TABLE user_blocks (
    blocker_id INTEGER NOT NULL REFERENCES users(id),
    blocked_id INTEGER NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (blocker_id, blocked_id)
);
CREATE INDEX idx_user_blocks_blocker ON user_blocks(blocker_id);
