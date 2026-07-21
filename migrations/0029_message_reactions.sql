-- Post reactions / upvotes (#94): one row per (message, user, kind), so a user
-- can hold at most one of each reaction kind on a post. Counts are aggregated
-- per message for display; the (user_id, created_at) index backs the per-user
-- rate-limit count.
CREATE TABLE message_reactions (
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (message_id, user_id, kind)
);

CREATE INDEX idx_reactions_message ON message_reactions(message_id);
CREATE INDEX idx_reactions_user_recent ON message_reactions(user_id, created_at);
