-- The federation moderation surface (epic #113, #112 Slice C).
--
-- Two pieces:
--
-- 1. Inbound reports. A remote instance can `Flag` content or an actor to us —
--    that's the fediverse's report mechanism, and it needs somewhere to land
--    where an operator will actually see it.
--
-- 2. Block severity. `ap_blocks` only knew allow/block; a real moderation stance
--    needs a middle setting. `suspend` is the hard block we already had (nothing
--    in or out, rejected at the door); `silence` still lets the domain federate
--    — existing follows and opted-in DMs keep working — but its content is no
--    longer accepted into shared surfaces (boards, timeline, mirrors).
--
-- Note neither is retroactive: blocking a peer stops what arrives next, it does
-- not delete what already arrived. `bbsctl ap-purge <domain>` is the explicit
-- tool for that.
CREATE TABLE ap_reports (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The Flag activity's own id, so a redelivered report lands once.
    ap_id           TEXT    UNIQUE,
    reporter_uri    TEXT    NOT NULL,
    reporter_handle TEXT    NOT NULL,
    -- The reported object URIs, one per line (a Flag may name several).
    objects         TEXT    NOT NULL,
    -- The reporter's comment, degraded to plain text like any remote content.
    content         TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    -- NULL while the report is open.
    resolved_at     INTEGER
);
-- Operators read the open ones first.
CREATE INDEX idx_ap_reports_open ON ap_reports (resolved_at, id DESC);

-- 'suspend' (the previous behavior) | 'silence'. Only meaningful for kind='block'.
ALTER TABLE ap_blocks ADD COLUMN severity TEXT NOT NULL DEFAULT 'suspend';
