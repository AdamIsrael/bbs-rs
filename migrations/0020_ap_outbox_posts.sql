-- Posts we publish into someone else's board (epic #113, #131).
--
-- These need their own table, and it's worth being clear why none of the
-- existing ones fit:
--
--   * `messages` is our boards. This post isn't on one of our boards — it's on
--     a remote board that is its own authority. Filing it there would make it
--     appear on a local board it doesn't belong to.
--   * `ap_board_posts` is the mirror: foreign objects we cache. Our own
--     not-yet-published post isn't one, and writing it there would assert the
--     remote board had published something it hasn't.
--
-- So this is the third thing: content we authored *for* a remote board, before
-- (and after) that board publishes it. It also supplies the post's permanent
-- `ap_id`, minted from this row's id — an ActivityPub object needs a stable URI
-- from the moment it's created, and this row is what makes ours stable.
--
-- The post becomes visible in the mirror the normal way: the board Announces it
-- back and it lands in `ap_board_posts` keyed by the same `ap_id`. Until then
-- the UI shows it from here, marked as awaiting the board — honest about the
-- fact that we are not the ones who decide it's published.
CREATE TABLE ap_outbox_posts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The Page's permanent URI, minted from `id` once known.
    ap_id       TEXT    UNIQUE,
    -- The remote board Group we published into.
    group_uri   TEXT    NOT NULL,
    -- The local author. Real FK: this is our content, by our user.
    author_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    subject     TEXT    NOT NULL,
    body        TEXT    NOT NULL,
    created_at  INTEGER NOT NULL
);
-- Listed per board, newest first, to show what's still awaiting publication.
CREATE INDEX idx_ap_outbox_posts_group ON ap_outbox_posts (group_uri, id DESC);
