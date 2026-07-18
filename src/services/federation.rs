//! ActivityPub actor identity: URI minting and keypairs (epic #113, #107).
//!
//! Two rules govern everything here.
//!
//! **URIs are permanent.** An ActivityPub `id` is a primary key across the
//! whole network. Once an actor or object has been delivered to a remote
//! server, its URI can never be rewritten — so URIs are only ever minted from
//! an origin that [`crate::config::Federation::origin`] has already validated,
//! and once a row has an `actor_uri` we never recompute it.
//!
//! **Local and remote actors share `users`.** A discovered remote actor is a
//! row keyed by a fully-qualified `alice@remote.social` handle with
//! `is_remote = 1` (see docs/FEDERATION.md). Registration rejects `@`, so the
//! two namespaces cannot collide.

use activitypub_federation::http_signatures::generate_actor_keypair;
use sqlx::sqlite::SqlitePool;

use crate::db::models::User;
use crate::error::{AppError, Result};

/// The validated public origin, and the URI layout built on it.
///
/// Construct via [`Origin::new`], which only accepts an origin that
/// [`crate::config::Federation::origin`] has approved — that's what keeps a
/// `https://localhost:8088` from ever reaching a minted URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin(String);

impl Origin {
    /// Build from an already-validated origin string.
    pub fn new(validated: impl Into<String>) -> Self {
        Self(validated.into())
    }

    /// Resolve straight from config, validating fail-closed.
    pub fn from_config(fed: &crate::config::Federation) -> anyhow::Result<Self> {
        Ok(Self::new(fed.origin()?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host part, as used in `acct:user@host` and WebFinger.
    pub fn host(&self) -> &str {
        self.0
            .split_once("//")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.0)
            .split('/')
            .next()
            .unwrap_or_default()
    }

    /// A local user's `Person` actor.
    pub fn person(&self, username: &str) -> String {
        format!("{}/u/{username}", self.0)
    }

    pub fn person_inbox(&self, username: &str) -> String {
        format!("{}/u/{username}/inbox", self.0)
    }

    pub fn person_outbox(&self, username: &str) -> String {
        format!("{}/u/{username}/outbox", self.0)
    }

    pub fn person_followers(&self, username: &str) -> String {
        format!("{}/u/{username}/followers", self.0)
    }

    /// The instance-wide shared inbox. Remote servers deliver one copy here for
    /// many local recipients instead of fanning out per-actor.
    pub fn shared_inbox(&self) -> String {
        format!("{}/inbox", self.0)
    }

    /// A board's `Group` actor (#111). Keyed by slug: `boards.name` is free
    /// text and not URI-safe.
    pub fn group(&self, slug: &str) -> String {
        format!("{}/c/{slug}", self.0)
    }

    /// A status (oneliner) `Note`.
    pub fn status(&self, id: i64) -> String {
        format!("{}/s/{id}", self.0)
    }

    /// The `Create` activity that wraps a status in an outbox.
    pub fn status_activity(&self, id: i64) -> String {
        format!("{}/s/{id}/activity", self.0)
    }

    /// The `acct:` URI a WebFinger query resolves, e.g. `acct:alice@bbs.example.com`.
    pub fn acct(&self, username: &str) -> String {
        format!("acct:{username}@{}", self.host())
    }
}

/// A local actor's stored identity.
#[derive(Debug, Clone)]
pub struct ActorKeys {
    pub actor_uri: String,
    pub public_key: String,
    pub private_key: String,
}

/// Fetch a local user's actor identity, generating and storing it on first use.
///
/// Generation is lazy rather than at registration: a board that never enables
/// federation should never hold RSA private keys, and the origin isn't known to
/// be valid until federation is switched on.
///
/// Idempotent — if a row already has an `actor_uri` we return it untouched.
/// Re-minting a URI would orphan every remote follow of that actor.
pub async fn ensure_person_keys(
    pool: &SqlitePool,
    origin: &Origin,
    user: &User,
) -> Result<ActorKeys> {
    if user.is_remote {
        // Remote actors' keys are theirs; we only ever store their *public*
        // half, fetched from their server.
        return Err(AppError::NotFound);
    }

    let existing: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT actor_uri, public_key, private_key FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(pool)
            .await?;

    if let Some((Some(actor_uri), Some(public_key), Some(private_key))) = existing {
        return Ok(ActorKeys {
            actor_uri,
            public_key,
            private_key,
        });
    }

    let keypair = generate_actor_keypair()
        .map_err(|e| AppError::Other(anyhow::anyhow!("generating actor keypair: {e}")))?;
    let actor_uri = origin.person(&user.username);
    let inbox = origin.person_inbox(&user.username);
    let shared_inbox = origin.shared_inbox();

    sqlx::query(
        "UPDATE users SET actor_uri = ?, inbox_url = ?, shared_inbox_url = ?, \
         public_key = ?, private_key = ? WHERE id = ?",
    )
    .bind(&actor_uri)
    .bind(&inbox)
    .bind(&shared_inbox)
    .bind(&keypair.public_key)
    .bind(&keypair.private_key)
    .bind(user.id)
    .execute(pool)
    .await?;

    tracing::info!("minted ActivityPub actor {actor_uri} for {}", user.username);
    Ok(ActorKeys {
        actor_uri,
        public_key: keypair.public_key,
        private_key: keypair.private_key,
    })
}

/// The ActivityStreams "public" collection. **Emit the full URI, not the
/// `as:Public` CURIE** — the short form is a known interop bug that makes posts
/// invisible on some servers.
pub const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// Record a status's permanent `ap_id`, if it doesn't have one yet.
///
/// Lazy, like actor keys: a board that never federates mints no URIs. The value
/// is derivable from the local id, but it's *stored* so it stays fixed even if
/// the configured origin later changes — the URI is already out in the world by
/// then.
pub async fn ensure_status_ap_id(pool: &SqlitePool, origin: &Origin, id: i64) -> Result<String> {
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT ap_id FROM oneliners WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    match existing {
        None => Err(AppError::NotFound),
        Some(Some(ap_id)) => Ok(ap_id),
        Some(None) => {
            let ap_id = origin.status(id);
            sqlx::query("UPDATE oneliners SET ap_id = ? WHERE id = ?")
                .bind(&ap_id)
                .bind(id)
                .execute(pool)
                .await?;
            Ok(ap_id)
        }
    }
}

/// Look up a local user by the username in an `acct:` URI, for WebFinger.
///
/// Only local, federatable accounts resolve: `guest` is shared (one keypair for
/// many humans means one abuse report suspends everyone), and remote rows
/// belong to other servers.
pub async fn find_local_actor(pool: &SqlitePool, username: &str) -> Result<Option<User>> {
    let user = crate::services::auth::find_user(pool, username).await?;
    Ok(user.filter(|u| !u.is_remote && !u.is_guest()))
}

/// The durable outbound delivery queue.
///
/// The AP crate ships its own queue, but it's **in-memory** and retries at
/// roughly 1min / 1hr / 2.5 days — a restart inside that window silently drops
/// deliveries, which is a known Lemmy pain point. Since we already have SQLite,
/// we persist instead and accept at-least-once semantics.
///
/// This is the storage half. The drain — signing each activity with its actor's
/// key and POSTing it — lands in #109, which is where the first real delivery
/// target appears: nothing can be delivered until an inbox accepts a `Follow`
/// and gives us a follower.
pub mod queue {
    use super::*;
    use crate::util::now_unix;

    /// A delivery waiting to go out.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Delivery {
        pub id: i64,
        /// Actor whose key signs the request.
        pub actor_uri: String,
        pub inbox_url: String,
        /// The serialized activity JSON.
        pub activity: String,
        pub attempts: i64,
    }

    /// Queue an activity for delivery to one inbox.
    ///
    /// One row per (activity, inbox): a `Create` going to 50 followers is 50
    /// rows, so one dead server can't stall the other 49.
    pub async fn enqueue(
        pool: &SqlitePool,
        actor_uri: &str,
        inbox_url: &str,
        activity: &str,
        activity_uri: Option<&str>,
    ) -> Result<i64> {
        let id = sqlx::query(
            "INSERT INTO ap_deliveries \
             (actor_uri, inbox_url, activity, activity_uri, next_attempt_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(actor_uri)
        .bind(inbox_url)
        .bind(activity)
        .bind(activity_uri)
        .bind(now_unix())
        .bind(now_unix())
        .execute(pool)
        .await?
        .last_insert_rowid();
        Ok(id)
    }

    /// Deliveries whose backoff has elapsed, oldest first.
    pub async fn due(pool: &SqlitePool, limit: i64) -> Result<Vec<Delivery>> {
        let rows = sqlx::query_as::<_, Delivery>(
            "SELECT id, actor_uri, inbox_url, activity, attempts FROM ap_deliveries \
             WHERE next_attempt_at <= ? ORDER BY id LIMIT ?",
        )
        .bind(now_unix())
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Drop a delivered activity. The queue holds only outstanding work — the
    /// audit trail of what we sent is the activity's own object.
    pub async fn mark_delivered(pool: &SqlitePool, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM ap_deliveries WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Exponential backoff for attempt `n` (1-based), capped at ~2.7 hours.
    ///
    /// Remote outages are routine on the fediverse, so retries are patient
    /// rather than aggressive: 1m, 2m, 4m … A caller that hammers a struggling
    /// server is how you get defederated.
    pub fn backoff_secs(attempts: i64) -> i64 {
        const BASE: i64 = 60;
        const CAP: i64 = 60 * 60 * 3;
        BASE.saturating_mul(
            1i64.checked_shl(attempts.clamp(0, 16) as u32)
                .unwrap_or(i64::MAX),
        )
        .min(CAP)
    }

    /// Record a failed attempt: back off, or give up past `max_attempts`.
    ///
    /// Returns `true` if the delivery was dropped for good. Giving up is normal
    /// — a peer can vanish permanently, and a queue that never forgets grows
    /// without bound.
    pub async fn mark_failed(
        pool: &SqlitePool,
        id: i64,
        error: &str,
        max_attempts: u32,
    ) -> Result<bool> {
        let attempts: Option<i64> =
            sqlx::query_scalar("SELECT attempts FROM ap_deliveries WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        let Some(attempts) = attempts else {
            return Ok(false);
        };
        let attempts = attempts + 1;
        if attempts >= max_attempts as i64 {
            tracing::warn!("giving up on delivery {id} after {attempts} attempts: {error}");
            mark_delivered(pool, id).await?;
            return Ok(true);
        }
        sqlx::query(
            "UPDATE ap_deliveries SET attempts = ?, next_attempt_at = ?, last_error = ? \
             WHERE id = ?",
        )
        .bind(attempts)
        .bind(now_unix() + backoff_secs(attempts))
        .bind(error)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(false)
    }

    /// How many deliveries are outstanding (operator visibility).
    pub async fn pending(pool: &SqlitePool) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM ap_deliveries")
            .fetch_one(pool)
            .await?)
    }
}

/// Domain-level federation policy, over the `ap_blocks` table.
///
/// Two postures (`[federation] allowlist_only`):
/// - **allowlist** (default): only domains with an `allow` row may federate.
///   For a small board this is a feature, not a limitation — open federation
///   means volunteering to moderate the entire internet.
/// - **blocklist**: anyone may federate except domains with a `block` row.
pub mod policy {
    use super::*;
    use crate::util::now_unix;

    /// Whether `domain` may federate with us under the given posture. Our own
    /// origin host is always allowed (we federate with ourselves).
    pub async fn domain_allowed(
        pool: &SqlitePool,
        origin_host: &str,
        domain: &str,
        allowlist_only: bool,
    ) -> Result<bool> {
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Ok(false);
        }
        if domain.eq_ignore_ascii_case(origin_host) {
            return Ok(true);
        }
        let kind = if allowlist_only { "allow" } else { "block" };
        let listed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_blocks WHERE kind = ? AND lower(domain) = ?",
        )
        .bind(kind)
        .bind(&domain)
        .fetch_one(pool)
        .await?;
        // Allowlist: must be listed. Blocklist: must NOT be listed.
        Ok(if allowlist_only {
            listed > 0
        } else {
            listed == 0
        })
    }

    /// Add or update a policy row (`kind` = "allow" | "block").
    pub async fn set(pool: &SqlitePool, domain: &str, kind: &str, reason: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO ap_blocks (domain, kind, reason, created_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(domain, kind) DO UPDATE SET reason = excluded.reason",
        )
        .bind(domain.trim().to_ascii_lowercase())
        .bind(kind)
        .bind(reason)
        .bind(now_unix())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Remove a policy row. Returns whether one existed.
    pub async fn unset(pool: &SqlitePool, domain: &str, kind: &str) -> Result<bool> {
        let n = sqlx::query("DELETE FROM ap_blocks WHERE lower(domain) = ? AND kind = ?")
            .bind(domain.trim().to_ascii_lowercase())
            .bind(kind)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// All policy rows of a kind, for operator display.
    pub async fn list(pool: &SqlitePool, kind: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT domain, reason FROM ap_blocks WHERE kind = ? ORDER BY domain",
        )
        .bind(kind)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}

/// Split a fediverse handle into `(user, domain)`. Accepts `alice@host`,
/// `@alice@host`, and `acct:alice@host`.
pub fn split_handle(handle: &str) -> Option<(&str, &str)> {
    let h = handle
        .trim()
        .trim_start_matches("acct:")
        .trim_start_matches('@');
    let (user, domain) = h.split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    Some((user, domain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new("https://bbs.example.com")
    }

    #[test]
    fn uris_are_built_from_the_origin() {
        let o = origin();
        assert_eq!(o.host(), "bbs.example.com");
        assert_eq!(o.person("alice"), "https://bbs.example.com/u/alice");
        assert_eq!(
            o.person_inbox("alice"),
            "https://bbs.example.com/u/alice/inbox"
        );
        assert_eq!(
            o.person_outbox("alice"),
            "https://bbs.example.com/u/alice/outbox"
        );
        assert_eq!(o.shared_inbox(), "https://bbs.example.com/inbox");
        assert_eq!(o.group("rust"), "https://bbs.example.com/c/rust");
        assert_eq!(o.acct("alice"), "acct:alice@bbs.example.com");
    }

    #[test]
    fn host_survives_a_dev_origin_with_a_port() {
        // debug_insecure origins keep their port; the host must still parse.
        let o = Origin::new("http://localhost:8088");
        assert_eq!(o.host(), "localhost:8088");
        assert_eq!(o.person("alice"), "http://localhost:8088/u/alice");
    }

    #[test]
    fn handles_split_in_every_form_we_might_be_handed() {
        assert_eq!(
            split_handle("alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("@alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("acct:alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("  alice@remote.social "),
            Some(("alice", "remote.social"))
        );

        for bad in ["alice", "", "@", "alice@", "@remote.social", "acct:"] {
            assert_eq!(split_handle(bad), None, "{bad:?} is not a handle");
        }
    }
}
