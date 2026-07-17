-- ActivityPub federation foundation (epic #113, phase #107).
--
-- Deliberately ADDITIVE ONLY: no table is rebuilt. `messages` is the content
-- table for the `messages_fts` FTS5 external-content index (0012), so
-- rebuilding it would mean dropping and recreating the virtual table and all
-- three sync triggers — the riskiest migration available to us. We avoid it by
-- representing remote actors as rows in `users`, keyed by a fully-qualified
-- `alice@remote.social` username (which is how fediverse handles are displayed
-- anyway). That keeps `messages.author_id NOT NULL REFERENCES users(id)` and
-- `users.username UNIQUE` exactly as they are — the domain lives inside the
-- handle, so it naturally prevents duplicate actors across domains.
--
-- Adding unrelated columns to a table under an external-content FTS index is
-- safe because the 0012 triggers name `new.id`/`new.subject`/`new.body`
-- explicitly. `tests/federation.rs` pins that.
--
-- See docs/FEDERATION.md.

-- --- Actors -----------------------------------------------------------------
-- Local users get an actor_uri + keypair when federation is enabled; remote
-- actors are discovered rows with is_remote = 1 and no usable password.
--
-- private_key is the FIRST secret-at-rest besides password_hash. It flows into
-- `bbsctl backup` (VACUUM INTO) and services::archive exports — that exposure
-- is documented in docs/FEDERATION.md, not accidental.
ALTER TABLE users ADD COLUMN actor_uri   TEXT;
ALTER TABLE users ADD COLUMN inbox_url   TEXT;
ALTER TABLE users ADD COLUMN shared_inbox_url TEXT;
ALTER TABLE users ADD COLUMN public_key  TEXT;
ALTER TABLE users ADD COLUMN private_key TEXT;
-- '' for local users; the remote host for federated actors.
ALTER TABLE users ADD COLUMN domain      TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN is_remote   INTEGER NOT NULL DEFAULT 0;
-- When we last refetched a remote actor's profile/keys (Unix seconds).
ALTER TABLE users ADD COLUMN actor_refreshed_at INTEGER;

-- An actor URI is globally unique. NULLs are distinct in SQLite, so every
-- existing local row (actor_uri IS NULL) is unaffected.
CREATE UNIQUE INDEX idx_users_actor_uri ON users (actor_uri);
CREATE INDEX idx_users_remote ON users (is_remote);

-- --- Objects ----------------------------------------------------------------
-- Messages get a global identity + cross-instance reply linkage. Local `id`
-- stays the primary key (the FTS index is keyed on it via content_rowid), so
-- the AP id URI is necessarily a secondary UNIQUE column.
ALTER TABLE messages ADD COLUMN ap_id           TEXT;
ALTER TABLE messages ADD COLUMN in_reply_to_uri TEXT;
CREATE UNIQUE INDEX idx_messages_ap_id ON messages (ap_id);

-- Oneliners become federated statuses (Notes) in #108.
ALTER TABLE oneliners ADD COLUMN ap_id TEXT;
CREATE UNIQUE INDEX idx_oneliners_ap_id ON oneliners (ap_id);

-- Boards become Group actors in #111. `name` is free text and not URI-safe, so
-- a slug is the portable handle; the keypair is the Group's actor identity.
ALTER TABLE boards ADD COLUMN slug        TEXT;
ALTER TABLE boards ADD COLUMN actor_uri   TEXT;
ALTER TABLE boards ADD COLUMN public_key  TEXT;
ALTER TABLE boards ADD COLUMN private_key TEXT;
CREATE UNIQUE INDEX idx_boards_slug ON boards (slug);
CREATE UNIQUE INDEX idx_boards_actor_uri ON boards (actor_uri);

-- --- Delivery ---------------------------------------------------------------
-- A durable outbound queue. The AP crate's built-in queue is in-memory and
-- retries at ~1min/1hr/2.5day, so a restart silently drops deliveries; we
-- persist instead. Drained by a background task modeled on ban_sweeper.
CREATE TABLE ap_deliveries (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Actor whose key signs the request (users.actor_uri or boards.actor_uri).
    actor_uri     TEXT    NOT NULL,
    -- Absolute inbox URL to POST to.
    inbox_url     TEXT    NOT NULL,
    -- The serialized activity JSON.
    activity      TEXT    NOT NULL,
    -- Dedup/debug handle for the activity's own id URI.
    activity_uri  TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,
    -- Unix seconds; the queue skips rows until now() >= next_attempt_at.
    next_attempt_at INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT,
    created_at    INTEGER NOT NULL
);
CREATE INDEX idx_ap_deliveries_due ON ap_deliveries (next_attempt_at, id);

-- --- Social graph -----------------------------------------------------------
-- Follows in both directions. `object_uri` is the followed actor (one of ours
-- for inbound, a remote one for outbound); `actor_uri` is the follower.
-- `state` is 'pending' | 'accepted'.
CREATE TABLE ap_follows (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_uri    TEXT    NOT NULL,
    object_uri   TEXT    NOT NULL,
    state        TEXT    NOT NULL DEFAULT 'pending',
    -- The remote Follow activity's id, needed to Accept/Undo it.
    follow_uri   TEXT,
    created_at   INTEGER NOT NULL,
    UNIQUE (actor_uri, object_uri)
);
CREATE INDEX idx_ap_follows_object ON ap_follows (object_uri, state);

-- --- Moderation -------------------------------------------------------------
-- Domain-level federation policy. Allowlist-by-default is the intended posture
-- (see docs/FEDERATION.md): open federation means volunteering to moderate the
-- entire internet. `kind` is 'allow' | 'block'.
CREATE TABLE ap_blocks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    domain     TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    reason     TEXT    NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE (domain, kind)
);
