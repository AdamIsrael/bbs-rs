-- Mirrored posts from followed remote boards (epic #113, phase #111 Slice C).
--
-- When a local user follows a remote board (a `Group`), that board `Announce`s
-- its posts to us. We degrade each `Page` to plain text and cache it here — the
-- read model for a subscribed remote board.
--
-- Kept separate from `messages` (our own boards) for the same reason the
-- timeline is separate from `oneliners`: these are foreign objects we mirror for
-- display, not posts we're the authority for. That also keeps `messages` and its
-- FTS index untouched. See docs/FEDERATION.md.
CREATE TABLE ap_board_posts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The Page's own `id` URI — globally unique, so a redelivery is a no-op.
    ap_id         TEXT    NOT NULL UNIQUE,
    -- The remote board's Group actor URI (what we followed) and its handle.
    group_uri     TEXT    NOT NULL,
    group_handle  TEXT    NOT NULL,
    -- The post's author (`attributedTo`) as a display handle.
    author_handle TEXT    NOT NULL,
    subject       TEXT    NOT NULL,
    -- The degraded plain-text body.
    content       TEXT    NOT NULL,
    -- The post's canonical HTML url, if any.
    url           TEXT,
    published     INTEGER NOT NULL,
    received_at   INTEGER NOT NULL
);
-- Browsed per board, newest first.
CREATE INDEX idx_ap_board_posts_group ON ap_board_posts (group_uri, published DESC, id DESC);
