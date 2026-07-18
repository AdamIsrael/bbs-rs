//! The `activitypub_federation` trait impls for inbound federation (#109).
//!
//! This is where we hand the crate what it needs to **verify HTTP signatures**:
//! given an inbound POST, it reads the signing actor's URL, fetches that actor
//! (via [`Object`]), and checks the request was signed by their key. We reuse
//! the crate's machinery here rather than hand-roll signatures — signature
//! verification is a security boundary full of subtle interop traps, and it's
//! the reason we took this dependency.
//!
//! [`FedActor`] is one type over both local and remote rows of the `users`
//! table: `read_from_id` finds either, and `from_json` persists a *remote*
//! actor as an `is_remote` shadow row (keyed by its `alice@remote.social`
//! handle — the same shape the rest of the codebase already assumes).

use activitypub_federation::config::{Data, FederationConfig, UrlVerifier};
use activitypub_federation::error::Error as ApFederationError;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::kinds::activity::{AcceptType, CreateType, FollowType, UndoType};
use activitypub_federation::kinds::actor::PersonType;
use activitypub_federation::protocol::public_key::PublicKey;
use activitypub_federation::protocol::verification::verify_domains_match;
use activitypub_federation::traits::{Activity, Actor, Object};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use std::time::Duration;
use url::Url;

use crate::config::Federation;
use crate::services::federation::{
    AS_CONTEXT, Origin, content, ensure_person_keys, follows, policy, queue, split_handle, timeline,
};
use crate::util::now_unix;

/// App state handed to the federation library. `Data<AppData>` derefs to this
/// in the inbox handler and every trait method.
#[derive(Clone)]
pub struct AppData {
    pub pool: SqlitePool,
    pub origin: Origin,
}

/// Error type for the federation HTTP surface. The crate's traits require an
/// error that is `From<activitypub_federation::error::Error>`; we also fold in
/// our own error types and render everything as a 500 (the crate's own example
/// does the same — inbound failures are logged, not detailed to the caller).
#[derive(Debug)]
pub struct ApError(pub anyhow::Error);

impl std::fmt::Display for ApError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ApError {}

impl From<anyhow::Error> for ApError {
    fn from(e: anyhow::Error) -> Self {
        ApError(e)
    }
}
// The crate's traits require the error be `From` the library error; the rest
// are for `?` in the trait method bodies.
impl From<ApFederationError> for ApError {
    fn from(e: ApFederationError) -> Self {
        ApError(e.into())
    }
}
impl From<sqlx::Error> for ApError {
    fn from(e: sqlx::Error) -> Self {
        ApError(e.into())
    }
}
impl From<url::ParseError> for ApError {
    fn from(e: url::ParseError) -> Self {
        ApError(e.into())
    }
}
// Our own service error, for `?` on `services::federation` calls in receive().
impl From<crate::error::AppError> for ApError {
    fn from(e: crate::error::AppError) -> Self {
        ApError(e.into())
    }
}

impl IntoResponse for ApError {
    fn into_response(self) -> Response {
        tracing::warn!("activitypub request failed: {:#}", self.0);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

/// An actor (`Person`), local or remote, as a row of the `users` table.
#[derive(Debug, Clone)]
pub struct FedActor {
    /// `users.username`: bare (`alice`) for local, a full handle
    /// (`alice@remote.social`) for remote.
    pub username: String,
    pub ap_id: ObjectId<FedActor>,
    pub inbox: Url,
    /// Always present — it's what verifies inbound signatures.
    pub public_key: String,
    /// Present only for local actors we sign *outbound* requests with.
    pub private_key: Option<String>,
    pub refreshed_at: DateTime<Utc>,
    pub local: bool,
}

/// The wire form of a `Person` actor. camelCase, like every AS2 object.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(rename = "type")]
    kind: PersonType,
    id: ObjectId<FedActor>,
    preferred_username: String,
    inbox: Url,
    public_key: PublicKey,
}

fn ts(secs: Option<i64>) -> DateTime<Utc> {
    DateTime::from_timestamp(secs.unwrap_or(0), 0).unwrap_or_else(Utc::now)
}

#[async_trait::async_trait]
impl Object for FedActor {
    type DataType = AppData;
    type Kind = Person;
    type Error = ApError;

    fn id(&self) -> &Url {
        self.ap_id.inner()
    }

    fn last_refreshed_at(&self) -> Option<DateTime<Utc>> {
        Some(self.refreshed_at)
    }

    /// Load an actor we already know — local or a previously-seen remote.
    /// Returning `None` tells the crate to fetch it over HTTP, landing in
    /// [`Object::from_json`].
    async fn read_from_id(
        object_id: Url,
        data: &Data<Self::DataType>,
    ) -> Result<Option<Self>, Self::Error> {
        let row: Option<(
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            Option<i64>,
        )> = sqlx::query_as(
            "SELECT username, inbox_url, public_key, private_key, is_remote, actor_refreshed_at \
                 FROM users WHERE actor_uri = ?",
        )
        .bind(object_id.as_str())
        .fetch_optional(&data.pool)
        .await?;
        let Some((username, inbox, public_key, private_key, is_remote, refreshed)) = row else {
            return Ok(None);
        };
        // A row with no key or inbox can't act as an actor yet (a local user
        // whose keys haven't been minted). Treat it as not-yet-an-actor.
        let (Some(inbox), Some(public_key)) = (inbox, public_key) else {
            return Ok(None);
        };
        Ok(Some(FedActor {
            username,
            ap_id: object_id.into(),
            inbox: Url::parse(&inbox)?,
            public_key,
            private_key,
            refreshed_at: ts(refreshed),
            local: !is_remote,
        }))
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        Ok(Person {
            kind: PersonType::Person,
            preferred_username: self
                .username
                .split('@')
                .next()
                .unwrap_or(&self.username)
                .to_string(),
            id: self.ap_id.clone(),
            inbox: self.inbox.clone(),
            public_key: self.public_key(),
        })
    }

    async fn verify(
        json: &Self::Kind,
        expected_domain: &Url,
        _data: &Data<Self::DataType>,
    ) -> Result<(), Self::Error> {
        // The actor's id must live on the host we fetched it from — this is
        // what stops one server from vouching for actors on another.
        verify_domains_match(json.id.inner(), expected_domain)?;
        Ok(())
    }

    /// Persist a freshly-fetched remote actor as an `is_remote` shadow row.
    ///
    /// The handle is `preferredUsername@domain`, matching how remote actors are
    /// stored everywhere else. The password hash is an unusable sentinel — a
    /// remote actor is not an account (auth rejects `is_remote` outright).
    async fn from_json(json: Self::Kind, data: &Data<Self::DataType>) -> Result<Self, Self::Error> {
        let ap_id = json.id.inner().clone();
        let domain = ap_id.host_str().unwrap_or_default().to_string();
        let handle = format!("{}@{}", json.preferred_username, domain);
        let inbox = json.inbox;
        let public_key = json.public_key.public_key_pem;
        let now = now_unix();

        // Keyed on actor_uri (globally unique). Re-fetches refresh the key and
        // inbox in place — actors rotate keys, and we must follow.
        sqlx::query(
            "INSERT INTO users \
               (username, password_hash, role, created_at, domain, is_remote, \
                actor_uri, inbox_url, public_key, actor_refreshed_at) \
             VALUES (?, '!', 'user', ?, ?, 1, ?, ?, ?, ?) \
             ON CONFLICT(actor_uri) DO UPDATE SET \
               inbox_url = excluded.inbox_url, \
               public_key = excluded.public_key, \
               actor_refreshed_at = excluded.actor_refreshed_at",
        )
        .bind(&handle)
        .bind(now)
        .bind(&domain)
        .bind(ap_id.as_str())
        .bind(inbox.as_str())
        .bind(&public_key)
        .bind(now)
        .execute(&data.pool)
        .await?;

        Ok(FedActor {
            username: handle,
            ap_id: ap_id.into(),
            inbox,
            public_key,
            private_key: None,
            refreshed_at: ts(Some(now)),
            local: false,
        })
    }
}

impl Actor for FedActor {
    fn public_key_pem(&self) -> &str {
        &self.public_key
    }

    fn private_key_pem(&self) -> Option<String> {
        self.private_key.clone()
    }

    fn inbox(&self) -> Url {
        self.inbox.clone()
    }
}

/// A remote actor's `Follow` of one of our local users.
///
/// `actor` is the remote follower; `object` is the local actor being followed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    #[serde(rename = "type")]
    kind: FollowType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: ObjectId<FedActor>,
}

/// Our `Accept` of a remote `Follow` — sent back through the delivery queue so
/// the remote server marks the follow relationship established.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Accept {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "type")]
    kind: AcceptType,
    id: String,
    actor: String,
    object: Follow,
}

/// An `Undo` of a prior activity. We only act on `Undo{Follow}` (an unfollow);
/// any other inner activity is accepted and ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    #[serde(rename = "type")]
    kind: UndoType,
    id: Url,
    actor: ObjectId<FedActor>,
    /// Mastodon embeds the full inner activity. We only understand a `Follow`
    /// here; anything else deserializes into the catch-all and is a no-op.
    object: UndoObject,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum UndoObject {
    Follow(Follow),
    Other(serde_json::Value),
}

/// A remote status delivered to us: `Create` wrapping a `Note`. Arrives because
/// a local user follows the author. We only read the fields a terminal timeline
/// needs; unmodeled fields are ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    #[serde(rename = "type")]
    kind: CreateType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: Note,
}

/// The inbound wire form of a `Note`. Distinct from the outbound
/// [`objects::Note`](crate::services::federation::objects::Note): here we accept
/// what Mastodon sends (HTML `content`, optional `url`/`published`) and are
/// lenient about missing fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    id: Url,
    attributed_to: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    published: Option<String>,
}

/// A remote server's `Accept` of a `Follow` we sent — confirmation that a local
/// user now follows a remote account. `object` is our original `Follow` echoed
/// back, so it carries who followed whom.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptFollow {
    #[serde(rename = "type")]
    kind: AcceptType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: Follow,
}

/// The catch-all for any activity we don't model yet — every well-formed one.
/// [`receive_activity`](activitypub_federation::axum::inbox::receive_activity)
/// still fetches the signing actor and verifies the HTTP signature before this
/// runs; we simply don't act on activities outside [`InboundActivity`]'s typed
/// arms.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnyActivity {
    id: Url,
    actor: ObjectId<FedActor>,
    #[serde(rename = "type")]
    kind: String,
    /// Keep unmodeled fields so nothing is silently dropped on the way through.
    #[serde(flatten)]
    rest: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Everything we're willing to receive at an inbox. `untagged` picks the first
/// variant whose shape matches: the typed activities have a fixed `type`
/// (`FollowType` only deserializes from `"Follow"`), so a `Follow` lands in
/// [`InboundActivity::Follow`] and anything unrecognized falls through to
/// [`InboundActivity::Other`]. New activity types are added as arms *above*
/// `Other`, never by touching the catch-all.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum InboundActivity {
    Follow(Follow),
    Undo(Undo),
    Create(Create),
    Accept(AcceptFollow),
    Other(AnyActivity),
}

#[async_trait::async_trait]
impl Activity for Follow {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Store the follow and queue an `Accept`. The signing actor (`actor`) and
    /// the followed actor (`object`) are already in the DB — the follower was
    /// persisted while verifying the signature, and the followed local user was
    /// minted when Mastodon fetched them before sending this. So both resolve
    /// from the local DB without a network round-trip.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let follower = self.actor.dereference_local(data).await?;
        let followed = self.object.dereference_local(data).await?;

        // Only local users can be followed. `read_from_id` returns remote rows
        // too, so guard here rather than trust the URI shape.
        if !followed.local {
            tracing::info!(
                "ignoring Follow of non-local actor {} from {}",
                followed.ap_id.inner(),
                follower.ap_id.inner()
            );
            return Ok(());
        }

        let row_id = follows::accept(
            &data.pool,
            follower.ap_id.inner().as_str(),
            followed.ap_id.inner().as_str(),
            self.id.as_str(),
        )
        .await?;

        // Echo the original Follow back inside the Accept, per convention.
        let accept = Accept {
            context: AS_CONTEXT,
            kind: AcceptType::Accept,
            id: format!("{}#accept/{row_id}", followed.ap_id.inner()),
            actor: followed.ap_id.inner().to_string(),
            object: self.clone(),
        };
        let activity = serde_json::to_string(&accept)
            .map_err(|e| ApError(anyhow::anyhow!("serializing Accept: {e}")))?;
        queue::enqueue(
            &data.pool,
            followed.ap_id.inner().as_str(),
            follower.inbox.as_str(),
            &activity,
            Some(&accept.id),
        )
        .await?;

        tracing::info!(
            "accepted Follow: {} now follows {}",
            follower.ap_id.inner(),
            followed.ap_id.inner()
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for Undo {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let UndoObject::Follow(follow) = self.object else {
            tracing::info!(
                "ignoring Undo of a non-Follow activity from {}",
                self.actor.inner()
            );
            return Ok(());
        };
        // The unfollow's authority is the Undo's own (signed) actor; only let it
        // undo its *own* follow, not someone else's.
        let follower = self.actor.inner().as_str();
        if follow.actor.inner().as_str() != follower {
            tracing::warn!("Undo actor {follower} does not match inner Follow actor; ignoring");
            return Ok(());
        }
        let removed = follows::remove(&data.pool, follower, follow.object.inner().as_str()).await?;
        if removed {
            tracing::info!(
                "removed follow: {follower} unfollowed {}",
                follow.object.inner()
            );
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for Create {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Cache a remote status for the timeline — but only if a local user follows
    /// its author, and only if the note's author is the actor who signed the
    /// delivery. The first check stops a followed account (or anyone) from
    /// spraying us with unrelated posts; the second stops an account from
    /// injecting notes attributed to someone else.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let author_uri = self.object.attributed_to.trim();
        if author_uri != self.actor.inner().as_str() {
            tracing::info!(
                "ignoring Create whose author {author_uri:?} isn't the signer {}",
                self.actor.inner()
            );
            return Ok(());
        }
        if !follows::is_followed_locally(&data.pool, author_uri).await? {
            tracing::debug!("dropping status from un-followed author {author_uri}");
            return Ok(());
        }

        let text = content::html_to_text(&self.object.content);
        // The author's handle: prefer the stored `user@host` from when we fetched
        // the actor; fall back to deriving it from the URI.
        let handle = author_handle(&data.pool, author_uri).await;
        let published = self
            .object
            .published
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or_else(now_unix);

        let fresh = timeline::insert(
            &data.pool,
            self.object.id.as_str(),
            author_uri,
            &handle,
            &text,
            self.object.url.as_deref(),
            published,
        )
        .await?;
        if fresh {
            tracing::info!("timeline: cached status {} from {handle}", self.object.id);
        }
        Ok(())
    }
}

/// The display handle for a remote actor: the stored `user@host` if we've seen
/// them, otherwise derived from the URI (`{last-path-segment}@{host}`).
async fn author_handle(pool: &SqlitePool, actor_uri: &str) -> String {
    if let Ok(Some(name)) =
        sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE actor_uri = ?")
            .bind(actor_uri)
            .fetch_optional(pool)
            .await
    {
        return name;
    }
    match Url::parse(actor_uri) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("unknown").to_string();
            let user = u
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown");
            format!("{user}@{host}")
        }
        Err(_) => actor_uri.to_string(),
    }
}

#[async_trait::async_trait]
impl Activity for AcceptFollow {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Mark our outbound follow accepted. The signer (`actor`) must be the
    /// account that was followed (`object.object`) — a server can only accept
    /// follows addressed to it, not vouch for someone else's.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let follower = self.object.actor.inner().as_str(); // our local user
        let followed = self.object.object.inner().as_str(); // the remote account
        if self.actor.inner().as_str() != followed {
            tracing::warn!(
                "Accept signer {} does not match the followed account {followed}; ignoring",
                self.actor.inner()
            );
            return Ok(());
        }
        if follows::mark_accepted(&data.pool, follower, followed).await? {
            tracing::info!("follow accepted: {follower} now follows {followed}");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for AnyActivity {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        // The signature is already verified by the time we get here. We accept
        // the activity but take no action on types we don't model yet.
        tracing::info!(
            "inbound {} from {} — signature verified, accepted (no handler)",
            self.kind,
            self.actor.inner()
        );
        Ok(())
    }
}

/// Delegate to whichever typed activity matched. Hand-written rather than macro
/// so the dispatch is explicit and greppable.
#[async_trait::async_trait]
impl Activity for InboundActivity {
    type DataType = AppData;
    type Error = ApError;

    fn id(&self) -> &Url {
        match self {
            InboundActivity::Follow(a) => a.id(),
            InboundActivity::Undo(a) => a.id(),
            InboundActivity::Create(a) => a.id(),
            InboundActivity::Accept(a) => a.id(),
            InboundActivity::Other(a) => a.id(),
        }
    }

    fn actor(&self) -> &Url {
        match self {
            InboundActivity::Follow(a) => a.actor(),
            InboundActivity::Undo(a) => a.actor(),
            InboundActivity::Create(a) => a.actor(),
            InboundActivity::Accept(a) => a.actor(),
            InboundActivity::Other(a) => a.actor(),
        }
    }

    async fn verify(&self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            InboundActivity::Follow(a) => a.verify(data).await,
            InboundActivity::Undo(a) => a.verify(data).await,
            InboundActivity::Create(a) => a.verify(data).await,
            InboundActivity::Accept(a) => a.verify(data).await,
            InboundActivity::Other(a) => a.verify(data).await,
        }
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            InboundActivity::Follow(a) => a.receive(data).await,
            InboundActivity::Undo(a) => a.receive(data).await,
            InboundActivity::Create(a) => a.receive(data).await,
            InboundActivity::Accept(a) => a.receive(data).await,
            InboundActivity::Other(a) => a.receive(data).await,
        }
    }
}

/// Enforces the domain allowlist/blocklist during every actor fetch and inbox
/// delivery. The crate calls [`UrlVerifier::verify`] on each URL it touches, so
/// a blocked sender's inbound POST fails when we try to fetch their actor to
/// check the signature.
#[derive(Clone)]
pub struct AllowlistVerifier {
    pool: SqlitePool,
    origin_host: String,
    allowlist_only: bool,
}

#[async_trait::async_trait]
impl UrlVerifier for AllowlistVerifier {
    async fn verify(&self, url: &Url) -> Result<(), ApFederationError> {
        let host = url.host_str().unwrap_or_default();
        let allowed =
            policy::domain_allowed(&self.pool, &self.origin_host, host, self.allowlist_only)
                .await
                .map_err(|e| ApFederationError::Other(format!("allowlist check failed: {e}")))?;
        if allowed {
            Ok(())
        } else {
            Err(ApFederationError::Other(format!(
                "domain {host} is not permitted to federate (allowlist policy)"
            )))
        }
    }
}

/// The delivery-queue drain — the outbound half of federation.
///
/// The crate ships an in-memory sender; we persist instead
/// ([`queue`](crate::services::federation::queue)) so a restart never silently
/// drops deliveries. This loop is the piece deferred from #108: it signs each
/// stored activity with its actor's key and POSTs it. Modeled on `ban_sweeper`
/// — a fixed tick, spawned once at startup.
pub async fn run_delivery_queue(
    config: FederationConfig<AppData>,
    interval: Duration,
    max_attempts: u32,
) {
    // A bounded batch keeps one slow tick from monopolizing the process; leftover
    // due rows are picked up on the next tick.
    const BATCH: i64 = 32;
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if let Err(e) = drain_once(&config, max_attempts, BATCH).await {
            tracing::warn!("delivery queue drain failed: {e:#}");
        }
    }
}

/// One drain pass: attempt every due delivery and update the queue for each.
/// Split from the loop so a test can drive a single deterministic pass.
pub async fn drain_once(
    config: &FederationConfig<AppData>,
    max_attempts: u32,
    batch: i64,
) -> anyhow::Result<()> {
    let data = config.to_request_data();
    let pool = data.pool.clone();
    for d in queue::due(&pool, batch).await? {
        match deliver_one(&data, &d).await {
            Ok(()) => queue::mark_delivered(&pool, d.id).await?,
            Err(e) => {
                let dropped =
                    queue::mark_failed(&pool, d.id, &format!("{e:#}"), max_attempts).await?;
                if !dropped {
                    tracing::debug!("delivery {} deferred after error: {e:#}", d.id);
                }
            }
        }
    }
    Ok(())
}

/// Sign and POST one queued delivery. The stored JSON is parsed back through
/// [`AnyActivity`] (its top-level `id`/`actor`/`type` are all the signer needs);
/// the crate builds the HTTP signature from the actor's key and the exact bytes
/// it re-serializes. An empty task list means the inbox was local or blocked by
/// the allowlist — treated as delivered, not retried forever.
async fn deliver_one(data: &Data<AppData>, d: &queue::Delivery) -> anyhow::Result<()> {
    use activitypub_federation::activity_sending::SendActivityTask;

    let actor_id = Url::parse(&d.actor_uri)?;
    let actor = FedActor::read_from_id(actor_id, data)
        .await
        .map_err(|e| anyhow::anyhow!("loading signing actor {}: {}", d.actor_uri, e.0))?
        .ok_or_else(|| anyhow::anyhow!("signing actor {} not found or unminted", d.actor_uri))?;
    if actor.private_key.is_none() {
        anyhow::bail!("signing actor {} has no private key", d.actor_uri);
    }

    let activity: AnyActivity = serde_json::from_str(&d.activity)
        .map_err(|e| anyhow::anyhow!("parsing queued activity {}: {e}", d.id))?;
    let inbox = Url::parse(&d.inbox_url)?;
    let tasks = SendActivityTask::prepare(&activity, &actor, vec![inbox], data)
        .await
        .map_err(|e| anyhow::anyhow!("preparing delivery {}: {e}", d.id))?;
    for task in tasks {
        task.sign_and_send(data)
            .await
            .map_err(|e| anyhow::anyhow!("delivering {}: {e}", d.id))?;
    }
    Ok(())
}

/// Follow a remote account (`user@host`) on behalf of a local user.
///
/// Resolves the handle via WebFinger (which fetches and caches the remote
/// actor), mints the local user's keypair so the delivery can be signed, records
/// a `pending` follow, and queues the signed `Follow`. The remote answers with
/// an `Accept` — handled by [`AcceptFollow`] — which flips the edge to
/// `accepted`. Returns the resolved remote handle for display.
pub async fn follow(
    data: &Data<AppData>,
    origin: &Origin,
    local_user: &crate::db::models::User,
    handle: &str,
) -> anyhow::Result<String> {
    use activitypub_federation::fetch::webfinger::webfinger_resolve_actor;

    let (user, domain) = split_handle(handle)
        .ok_or_else(|| anyhow::anyhow!("{handle:?} is not a user@host handle"))?;
    // Mint the local actor first: the drain signs the Follow with its key.
    let keys = ensure_person_keys(&data.pool, origin, local_user).await?;
    let local_uri = keys.actor_uri;

    let remote: FedActor =
        webfinger_resolve_actor::<AppData, FedActor>(&format!("{user}@{domain}"), data)
            .await
            .map_err(|e| anyhow::anyhow!("resolving {handle}: {}", e.0))?;
    let remote_uri = remote.ap_id.inner().to_string();

    let follow_id = format!("{local_uri}#follow/{}", now_unix());
    let activity = serde_json::json!({
        "@context": AS_CONTEXT,
        "id": follow_id,
        "type": "Follow",
        "actor": local_uri,
        "object": remote_uri,
    })
    .to_string();

    follows::request(&data.pool, &local_uri, &remote_uri, &follow_id).await?;
    queue::enqueue(
        &data.pool,
        &local_uri,
        remote.inbox.as_str(),
        &activity,
        Some(&follow_id),
    )
    .await?;
    Ok(remote.username)
}

/// Unfollow a remote account a local user follows. Drops the local edge and
/// queues a signed `Undo{Follow}` so the remote stops delivering. Returns
/// whether a follow existed. Uses the cached actor row — no network fetch.
pub async fn unfollow(
    data: &Data<AppData>,
    origin: &Origin,
    local_user: &crate::db::models::User,
    handle: &str,
) -> anyhow::Result<bool> {
    let local_uri = origin.person(&local_user.username);
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT actor_uri, inbox_url FROM users WHERE username = ? AND is_remote = 1",
    )
    .bind(handle)
    .fetch_optional(&data.pool)
    .await?;
    let Some((remote_uri, Some(inbox))) = row else {
        return Ok(false);
    };

    // Echo the original Follow's id when we have it, so the remote can match the
    // Undo to the exact follow.
    let follow_uri: Option<String> = sqlx::query_scalar(
        "SELECT follow_uri FROM ap_follows WHERE actor_uri = ? AND object_uri = ?",
    )
    .bind(&local_uri)
    .bind(&remote_uri)
    .fetch_optional(&data.pool)
    .await?
    .flatten();

    if !follows::remove(&data.pool, &local_uri, &remote_uri).await? {
        return Ok(false);
    }

    let undo_id = format!("{local_uri}#unfollow/{}", now_unix());
    let inner_follow_id = follow_uri.unwrap_or_else(|| format!("{local_uri}#follow"));
    let activity = serde_json::json!({
        "@context": AS_CONTEXT,
        "id": undo_id,
        "type": "Undo",
        "actor": local_uri,
        "object": {
            "id": inner_follow_id,
            "type": "Follow",
            "actor": local_uri,
            "object": remote_uri,
        },
    })
    .to_string();
    queue::enqueue(&data.pool, &local_uri, &inbox, &activity, Some(&undo_id)).await?;
    Ok(true)
}

/// Follow a remote account, building the federation config from settings.
///
/// The convenience entry point for callers that hold `[federation]` settings but
/// not a live `FederationConfig` — the in-BBS follow action and `bbsctl`. It
/// validates that federation is on, builds a short-lived config for the network
/// fetch, and delegates to [`follow`].
pub async fn follow_handle(
    pool: &SqlitePool,
    fed: &Federation,
    local_user: &crate::db::models::User,
    handle: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(fed.enabled, "federation is not enabled");
    let origin = Origin::from_config(fed)?;
    let config = build_config(pool.clone(), origin.clone(), fed).await?;
    follow(&config.to_request_data(), &origin, local_user, handle).await
}

/// Unfollow a remote account, building the federation config from settings. The
/// settings-holding counterpart to [`unfollow`].
pub async fn unfollow_handle(
    pool: &SqlitePool,
    fed: &Federation,
    local_user: &crate::db::models::User,
    handle: &str,
) -> anyhow::Result<bool> {
    anyhow::ensure!(fed.enabled, "federation is not enabled");
    let origin = Origin::from_config(fed)?;
    let config = build_config(pool.clone(), origin.clone(), fed).await?;
    unfollow(&config.to_request_data(), &origin, local_user, handle).await
}

/// Build the federation library config from our validated settings.
///
/// `debug_insecure` maps to the crate's debug mode (permits http/localhost) so
/// two instances can federate on one machine; it is never on for a real board.
pub async fn build_config(
    pool: SqlitePool,
    origin: Origin,
    fed: &Federation,
) -> anyhow::Result<FederationConfig<AppData>> {
    let origin_host = origin.host().to_string();
    let verifier = AllowlistVerifier {
        pool: pool.clone(),
        origin_host,
        allowlist_only: fed.allowlist_only,
    };
    let app_data = AppData { pool, origin };
    let config = FederationConfig::builder()
        .domain(app_data.origin.host().to_string())
        .app_data(app_data)
        .url_verifier(Box::new(verifier))
        .debug(fed.debug_insecure)
        .build()
        .await?;
    Ok(config)
}
