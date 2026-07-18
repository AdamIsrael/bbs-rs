-- Inbound remote statuses: the timeline (epic #113, phase #109 Slice C).
--
-- When a local user follows a remote account, that account delivers its
-- statuses (`Create{Note}`) to our inbox. We degrade each Note's HTML to plain
-- text (a terminal can't render images or markup) and store it here — the
-- read model behind the timeline screen.
--
-- Kept separate from `oneliners` on purpose: oneliners are *our* posts (they
-- mint permanent URIs and federate outward), whereas these are foreign objects
-- we cache for display. Mixing them would blur which posts we're the authority
-- for. See docs/FEDERATION.md.
CREATE TABLE ap_timeline (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The Note's own `id` URI — globally unique, so a redelivery is a no-op.
    ap_id         TEXT    NOT NULL UNIQUE,
    -- The author's actor URI (`attributedTo`) and display handle (`user@host`).
    author_uri    TEXT    NOT NULL,
    author_handle TEXT    NOT NULL,
    -- The degraded plain-text body (HTML → text, images → `[img: alt]`).
    content       TEXT    NOT NULL,
    -- The Note's canonical HTML url, if it carried one (for "open in browser").
    url           TEXT,
    -- The Note's own `published` time, Unix seconds (falls back to receipt).
    published     INTEGER NOT NULL,
    received_at   INTEGER NOT NULL
);
-- The timeline is read newest-first.
CREATE INDEX idx_ap_timeline_published ON ap_timeline (published DESC, id DESC);
