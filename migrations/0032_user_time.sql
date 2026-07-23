-- Per-user daily time limits (#75): connected seconds accumulated per user per
-- day. `day` is the UTC day number (unix seconds / 86400), so a row is one
-- user's usage for one day; a finished session adds its duration here.
CREATE TABLE user_time (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    day     INTEGER NOT NULL,
    seconds INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, day)
);

CREATE INDEX idx_user_time_day ON user_time(day);
