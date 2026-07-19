-- Sysop broadcasts (#69): a durable hand-off so `bbsctl broadcast "<msg>"`,
-- running in a separate process, can reach the server's live sessions.
--
-- The server's maintenance sweeper polls this table and fans each new row out
-- to every connected session as a transient toast (it is not re-shown per
-- session, and nothing keys off delivery — the row is just the message + when).
-- In-BBS admin broadcasts skip the table and fan out immediately, exactly as
-- in-BBS bans kick without waiting for the sweeper.
CREATE TABLE broadcasts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    text       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);
