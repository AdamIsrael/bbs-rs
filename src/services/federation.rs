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
