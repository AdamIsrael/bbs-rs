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
use activitypub_federation::kinds::activity::{
    AcceptType, AnnounceType, CreateType, DeleteType, FlagType, FollowType, UndoType, UpdateType,
};
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
    AS_CONTEXT, Origin, PUBLIC, content, ensure_person_keys, follows, lifecycle, mirror,
    moderation, policy, queue, split_handle, timeline,
};
use crate::util::{now_unix, reply_subject};

/// App state handed to the federation library. `Data<AppData>` derefs to this
/// in the inbox handler and every trait method.
#[derive(Clone)]
pub struct AppData {
    pub pool: SqlitePool,
    pub origin: Origin,
    /// Whether to accept inbound remote DMs into the mailbox (#110). Snapshotted
    /// at startup from `[federation] allow_remote_dms`, like the rest of the
    /// federation config — toggling it needs a restart.
    pub allow_remote_dms: bool,
    /// Rate limits, reused as the **inbound** flood guard for remote authors
    /// posting into our boards (#112). Snapshotted at startup like the rest.
    pub limits: crate::config::Limits,
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

/// The wire form of an actor we fetch. Named `Person` for history, but `kind`
/// is a lenient `String` so a **`Group`** (a remote board, #111) deserializes
/// through the same path — both carry `preferredUsername`, `inbox`, `publicKey`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(rename = "type")]
    kind: String,
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
        if let Some((username, inbox, public_key, private_key, is_remote, refreshed)) = row {
            // A row with no key or inbox can't act as an actor yet (a local user
            // whose keys haven't been minted). Treat it as not-yet-an-actor.
            let (Some(inbox), Some(public_key)) = (inbox, public_key) else {
                return Ok(None);
            };
            return Ok(Some(FedActor {
                username,
                ap_id: object_id.into(),
                inbox: Url::parse(&inbox)?,
                public_key,
                private_key,
                refreshed_at: ts(refreshed),
                local: !is_remote,
            }));
        }

        // Not a user — try a board `Group` (#111). Its keys live in `boards`;
        // the inbox URL is derived from the slug, not stored. Groups are always
        // local, so this both signs their outbound Announce/Accept and lets an
        // inbound `Follow` of a board resolve its target.
        let board: Option<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT slug, public_key, private_key FROM boards WHERE actor_uri = ?")
                .bind(object_id.as_str())
                .fetch_optional(&data.pool)
                .await?;
        let Some((slug, Some(public_key), private_key)) = board else {
            return Ok(None);
        };
        Ok(Some(FedActor {
            username: slug.clone(),
            inbox: Url::parse(&data.origin.group_inbox(&slug))?,
            ap_id: object_id.into(),
            public_key,
            private_key,
            refreshed_at: Utc::now(),
            local: true,
        }))
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        Ok(Person {
            kind: "Person".to_string(),
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
                actor_uri, inbox_url, public_key, actor_refreshed_at, actor_kind) \
             VALUES (?, '!', 'user', ?, ?, 1, ?, ?, ?, ?, ?) \
             ON CONFLICT(actor_uri) DO UPDATE SET \
               inbox_url = excluded.inbox_url, \
               public_key = excluded.public_key, \
               actor_refreshed_at = excluded.actor_refreshed_at, \
               actor_kind = excluded.actor_kind",
        )
        .bind(&handle)
        .bind(now)
        .bind(&domain)
        .bind(ap_id.as_str())
        .bind(inbox.as_str())
        .bind(&public_key)
        .bind(now)
        // Straight from the fetched document, which is why `Person::kind` is a
        // lenient String — a `Group` (remote board, #111) arrives through this
        // same path and is what the mirrored-boards screen lists (#132).
        .bind(&json.kind)
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
    /// A content warning / subject line, when present.
    #[serde(default)]
    summary: Option<String>,
    /// A `Page`'s title — a board post's subject.
    #[serde(default)]
    name: Option<String>,
    /// The board this post belongs to (FEP-1b12). **This is what routes an
    /// inbound post to a board**, even when it was delivered to a *person's*
    /// inbox rather than the Group's — the case Mastodon replies hit (#112).
    #[serde(default)]
    audience: Option<String>,
    /// The post this replies to, by its `ap_id` — threads a remote reply under
    /// its parent.
    #[serde(default)]
    in_reply_to: Option<String>,
    /// Addressing. A note with a local actor in `to`/`cc` but **not** the Public
    /// collection is a direct message; anything Public is a status.
    #[serde(default, deserialize_with = "string_or_seq")]
    to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_seq")]
    cc: Vec<String>,
}

/// AS2 addressing fields may be a single string or an array of strings; accept
/// both so `"to": "…#Public"` and `"to": ["…"]` both parse.
fn string_or_seq<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
        Null,
    }
    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
        OneOrMany::Null => Vec::new(),
    })
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

/// A board post as a remote instance sent it: a `Page` inside a `Create`. Only
/// the fields the mirror displays; lenient about the rest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncedPage {
    id: Url,
    attributed_to: String,
    #[serde(default)]
    name: Option<String>,
    /// Set when the announced object is a reply `Note` (#139).
    #[serde(default)]
    in_reply_to: Option<String>,
    #[serde(default)]
    content: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    published: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncedCreate {
    #[serde(rename = "type")]
    kind: CreateType,
    object: AnnouncedPage,
}

/// What a board Group relays to its subscribers. A new post is the common case,
/// but a board also propagates its members' withdrawals and edits so a post
/// deleted upstream doesn't linger in every subscriber's mirror (#133).
///
/// `untagged` is safe here for the same reason it is on [`InboundActivity`]:
/// each variant's `type` is a strict unit type, so exactly one can match. An
/// `Announce` wrapping anything else falls to `Other` and is logged, not
/// silently dropped.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AnnouncedObject {
    Create(AnnouncedCreate),
    Delete(Delete),
    Update(Update),
    Other(serde_json::Value),
}

/// A remote board Group's `Announce{…}` — how a followed board syndicates
/// activity to us (FEP-1b12, #111). The Group's signature is the authority for
/// *relaying*; what that authority extends to depends on the inner activity —
/// see [`lifecycle::delete_announced`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Announce {
    #[serde(rename = "type")]
    kind: AnnounceType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: AnnouncedObject,
}

/// An object reference: either a bare URI or an embedded object with an `id`.
/// `Delete` sends the former, `Update` the latter.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum ObjectRef {
    Uri(String),
    Embedded {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        content: String,
    },
}

impl ObjectRef {
    fn id(&self) -> &str {
        match self {
            ObjectRef::Uri(u) => u,
            ObjectRef::Embedded { id, .. } => id,
        }
    }
}

/// A remote author withdrawing something they sent us (#112).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delete {
    #[serde(rename = "type")]
    kind: DeleteType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: ObjectRef,
}

/// A remote author editing something they sent us (#112).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    #[serde(rename = "type")]
    kind: UpdateType,
    id: Url,
    actor: ObjectId<FedActor>,
    object: ObjectRef,
}

/// A remote instance reporting content or an actor to us — the fediverse's
/// report mechanism (#112). `object` may name several things at once.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Flag {
    #[serde(rename = "type")]
    kind: FlagType,
    id: Url,
    actor: ObjectId<FedActor>,
    #[serde(default, deserialize_with = "string_or_seq")]
    object: Vec<String>,
    #[serde(default)]
    content: Option<String>,
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
    Announce(Announce),
    Delete(Delete),
    Update(Update),
    Flag(Flag),
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
        let author_uri = self.object.attributed_to.trim().to_string();
        if author_uri != self.actor.inner().as_str() {
            tracing::info!(
                "ignoring Create whose author {author_uri:?} isn't the signer {}",
                self.actor.inner()
            );
            return Ok(());
        }

        // A post carrying `audience` (or addressed to) one of our board Groups
        // belongs on that board — checked *first*, and by audience rather than
        // delivery path, so a reply that lands in a person's inbox still reaches
        // the board (the case Mastodon replies hit; #112).
        if let Some(board_id) = self.board_target(&data.pool).await? {
            return self.receive_board_post(data, &author_uri, board_id).await;
        }

        // A note addressed to a local actor but not the Public collection is a
        // direct message; route it to the mailbox instead of the timeline.
        let is_public = self
            .object
            .to
            .iter()
            .chain(&self.object.cc)
            .any(|a| a == PUBLIC);
        if !is_public && let Some(recipient_id) = self.local_recipient(&data.pool).await? {
            return self.receive_dm(data, &author_uri, recipient_id).await;
        }

        // Otherwise a public status — cached only from accounts a local user
        // follows.
        if !follows::is_followed_locally(&data.pool, &author_uri).await? {
            tracing::debug!("dropping status from un-followed author {author_uri}");
            return Ok(());
        }
        if policy::actor_silenced(&data.pool, &author_uri).await? {
            tracing::info!("dropping status from silenced {author_uri}");
            return Ok(());
        }
        let text = content::html_to_text(&self.object.content);
        let handle = author_handle(&data.pool, &author_uri).await;
        let published = self.published_unix();
        let fresh = timeline::insert(
            &data.pool,
            self.object.id.as_str(),
            &author_uri,
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

impl Create {
    /// The note's `published` time as Unix seconds, falling back to now.
    fn published_unix(&self) -> i64 {
        self.object
            .published
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or_else(now_unix)
    }

    /// The local board this post belongs to, if any: `audience` first (the
    /// FEP-1b12 signal), then `to`/`cc`.
    async fn board_target(&self, pool: &SqlitePool) -> Result<Option<i64>, ApError> {
        if let Some(audience) = self.object.audience.as_deref()
            && let Some(id) =
                crate::services::federation::find_board_by_actor_uri(pool, audience.trim()).await?
        {
            return Ok(Some(id));
        }
        for uri in self.object.to.iter().chain(&self.object.cc) {
            if let Some(id) =
                crate::services::federation::find_board_by_actor_uri(pool, uri).await?
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Accept a post from a remote instance into one of our boards, then
    /// re-`Announce` it — we're the board's home, so subscribers hear it from us.
    ///
    /// Content is **degraded to plain text on the way in**, which is also our
    /// sanitization: no remote HTML is ever stored or rendered (the federation
    /// crate does none of its own).
    async fn receive_board_post(
        self,
        data: &Data<AppData>,
        author_uri: &str,
        board_id: i64,
    ) -> Result<(), ApError> {
        let author_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM users WHERE actor_uri = ? AND is_remote = 1")
                .bind(author_uri)
                .fetch_optional(&data.pool)
                .await?;
        let Some(author_id) = author_id else {
            tracing::warn!("board post from unknown actor {author_uri}; ignoring");
            return Ok(());
        };
        // A silenced domain may still federate, but its content stays out of
        // shared surfaces like boards (#112).
        if policy::actor_silenced(&data.pool, author_uri).await? {
            tracing::info!("dropping board post from silenced {author_uri}");
            return Ok(());
        }

        // Inbound flood guard: a remote author gets the same per-window post
        // budget as a local one. A remote server enforces its own limits, but we
        // don't take its word for it.
        if let Some(since) = data.limits.window_start(now_unix())
            && data.limits.max_posts > 0
        {
            let recent =
                crate::services::boards::author_post_count_since(&data.pool, author_id, since)
                    .await?;
            if recent >= data.limits.max_posts as i64 {
                tracing::warn!(
                    "dropping board post from {author_uri}: over the inbound rate limit"
                );
                return Ok(());
            }
        }

        let body = content::html_to_text(&self.object.content);
        let subject = self
            .object
            .name
            .as_deref()
            .map(content::html_to_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no subject)".to_string());

        let stored = crate::services::boards::store_remote_post(
            &data.pool,
            board_id,
            author_id,
            &subject,
            &body,
            self.object.id.as_str(),
            self.object.in_reply_to.as_deref(),
        )
        .await?;
        let Some(message_id) = stored else {
            tracing::debug!("board post {} already stored", self.object.id);
            return Ok(());
        };
        tracing::info!(
            "board: accepted post {} from {author_uri} into board {board_id}",
            self.object.id
        );

        // Re-Announce as the Group so every subscriber sees it — including the
        // instance that sent it, which dedups on the post's id.
        match crate::services::federation::outbound::deliver_board_post(
            &data.pool,
            &data.origin,
            message_id,
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("re-announced inbound post {message_id} to {n} inbox(es)"),
            Err(e) => tracing::warn!("could not re-announce inbound post {message_id}: {e}"),
        }
        Ok(())
    }

    /// The id of a local, non-guest user this note is addressed to, if any.
    async fn local_recipient(&self, pool: &SqlitePool) -> Result<Option<i64>, ApError> {
        for uri in self.object.to.iter().chain(&self.object.cc) {
            let row: Option<(i64,)> = sqlx::query_as(
                "SELECT id FROM users WHERE actor_uri = ? AND is_remote = 0 AND role != 'guest'",
            )
            .bind(uri)
            .fetch_optional(pool)
            .await?;
            if let Some((id,)) = row {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Store an inbound direct message in the recipient's mailbox — behind the
    /// `allow_remote_dms` opt-in. Fediverse DMs are not private; the mailbox UI
    /// labels these, and a board that hasn't opted in drops them silently.
    async fn receive_dm(
        self,
        data: &Data<AppData>,
        author_uri: &str,
        recipient_id: i64,
    ) -> Result<(), ApError> {
        if !data.allow_remote_dms {
            tracing::info!("dropping inbound DM from {author_uri} (allow_remote_dms is off)");
            return Ok(());
        }
        let from_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM users WHERE actor_uri = ? AND is_remote = 1")
                .bind(author_uri)
                .fetch_optional(&data.pool)
                .await?;
        let Some(from_id) = from_id else {
            tracing::warn!("inbound DM from unknown actor {author_uri}; ignoring");
            return Ok(());
        };
        let body = content::html_to_text(&self.object.content);
        let subject = self
            .object
            .summary
            .as_deref()
            .map(content::html_to_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Direct message".to_string());
        let fresh = crate::services::mail::store_inbound_remote(
            &data.pool,
            from_id,
            recipient_id,
            &subject,
            &body,
            self.object.id.as_str(),
        )
        .await?;
        if fresh {
            tracing::info!(
                "mailbox: stored inbound DM {} from {author_uri}",
                self.object.id
            );
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
impl Activity for Announce {
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

    /// Handle what a followed remote board relays. The **Group's** signature is
    /// the authority for the relay itself (it's the signer, `self.actor`), so
    /// subscription to that Group gates everything below — but what the Group is
    /// allowed to *do* differs per inner activity, so each arm authorizes on its
    /// own terms.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let group_uri = self.actor.inner().as_str();
        if !follows::is_followed_locally(&data.pool, group_uri).await? {
            tracing::debug!("dropping Announce from un-followed board {group_uri}");
            return Ok(());
        }
        if policy::actor_silenced(&data.pool, group_uri).await? {
            tracing::info!("dropping Announce from silenced board {group_uri}");
            return Ok(());
        }

        let create = match self.object {
            AnnouncedObject::Create(c) => c,
            AnnouncedObject::Delete(d) => return d.receive_announced(data, group_uri).await,
            AnnouncedObject::Update(u) => return u.receive_announced(data, group_uri).await,
            AnnouncedObject::Other(v) => {
                let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                tracing::info!("Announce{{{kind}}} from {group_uri} has no handler; ignoring");
                return Ok(());
            }
        };
        let page = create.object;
        let group_handle = author_handle(&data.pool, group_uri).await;
        let author_handle = author_handle(&data.pool, page.attributed_to.trim()).await;
        let content = content::html_to_text(&page.content);
        let in_reply_to = page
            .in_reply_to
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        // A bbs-rs peer sends `name` even on a reply Note, so BBS↔BBS keeps the
        // real subject. Everyone else omits it — hence the `Re: <parent>` fallback
        // for a reply whose parent we hold, and only then "(untitled)".
        let subject = match page
            .name
            .map(|n| content::html_to_text(&n))
            .filter(|n| !n.is_empty())
        {
            Some(name) => name,
            None => match in_reply_to {
                Some(parent) => mirror::subject_of(&data.pool, parent)
                    .await?
                    .map(|s| reply_subject(&s))
                    .unwrap_or_else(|| "(untitled reply)".to_string()),
                None => "(untitled)".to_string(),
            },
        };
        let published = page
            .published
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or_else(now_unix);

        let fresh = mirror::insert(
            &data.pool,
            page.id.as_str(),
            group_uri,
            &group_handle,
            &author_handle,
            page.attributed_to.trim(),
            &subject,
            &content,
            page.url.as_deref(),
            published,
            in_reply_to,
        )
        .await?;
        if fresh {
            tracing::info!("mirror: cached board post {} from {group_handle}", page.id);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for Delete {
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

    /// Withdraw content on its owner's instruction. Authorization lives in the
    /// SQL: an actor can only delete rows it owns, so an unknown object and
    /// someone else's object are indistinguishable here — both are a no-op.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let actor = self.actor.inner().as_str();
        match lifecycle::delete(&data.pool, actor, self.object.id()).await? {
            Some(target) => tracing::info!(
                "honored Delete of {} from {actor} ({target:?})",
                self.object.id()
            ),
            None => tracing::debug!(
                "Delete of {} from {actor} matched nothing we hold",
                self.object.id()
            ),
        }
        Ok(())
    }
}

impl Delete {
    /// The `Announce{Delete}` path (#133): a board propagating one of its
    /// members' withdrawals to subscribers.
    ///
    /// Scoped to posts this Group announced, and refused outright for content
    /// we're the authority for — a remote board doesn't get to delete a post on
    /// one of *our* boards just because it can name its URI.
    async fn receive_announced(self, data: &Data<AppData>, group_uri: &str) -> Result<(), ApError> {
        let actor = self.actor.inner().as_str();
        let object_id = self.object.id();
        if lifecycle::delete_announced(&data.pool, group_uri, actor, object_id).await? {
            tracing::info!("honored relayed Delete of {object_id} announced by {group_uri}");
        } else if lifecycle::announced_hits_local_content(&data.pool, object_id).await? {
            tracing::warn!(
                "refused relayed Delete of {object_id} from board {group_uri}: \
                 that object is ours, not the board's to withdraw"
            );
        } else {
            tracing::debug!("relayed Delete of {object_id} from {group_uri} matched nothing");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for Update {
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

    /// Apply a remote edit, degrading the new content exactly like the original.
    /// A bare-URI `Update` carries nothing to apply, so it's ignored.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let actor = self.actor.inner().as_str();
        let ObjectRef::Embedded { id, name, content } = &self.object else {
            tracing::debug!("Update from {actor} carried no object body; ignoring");
            return Ok(());
        };
        let body = content::html_to_text(content);
        let subject = name
            .as_deref()
            .map(content::html_to_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no subject)".to_string());
        match lifecycle::update(&data.pool, actor, id, &subject, &body).await? {
            Some(target) => tracing::info!("honored Update of {id} from {actor} ({target:?})"),
            None => tracing::debug!("Update of {id} from {actor} matched nothing we hold"),
        }
        Ok(())
    }
}

impl Update {
    /// The `Announce{Update}` path (#133) — a board propagating a member's edit.
    /// Same authorization rules as [`Delete::receive_announced`].
    async fn receive_announced(self, data: &Data<AppData>, group_uri: &str) -> Result<(), ApError> {
        let actor = self.actor.inner().as_str();
        let ObjectRef::Embedded { id, name, content } = &self.object else {
            tracing::debug!("relayed Update from {group_uri} carried no object body; ignoring");
            return Ok(());
        };
        let body = content::html_to_text(content);
        let subject = name
            .as_deref()
            .map(content::html_to_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no subject)".to_string());
        if lifecycle::update_announced(&data.pool, group_uri, actor, id, &subject, &body).await? {
            tracing::info!("honored relayed Update of {id} announced by {group_uri}");
        } else if lifecycle::announced_hits_local_content(&data.pool, id).await? {
            tracing::warn!(
                "refused relayed Update of {id} from board {group_uri}: \
                 that object is ours, not the board's to edit"
            );
        } else {
            tracing::debug!("relayed Update of {id} from {group_uri} matched nothing");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Activity for Flag {
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

    /// File a remote report for an operator to read. Reports are recorded, never
    /// acted on automatically — deciding what a report means is a human call,
    /// and auto-acting would hand any peer a remote moderation lever.
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        let reporter = self.actor.inner().as_str();
        let handle = author_handle(&data.pool, reporter).await;
        let objects = self.object.join("\n");
        let comment =
            self.object.is_empty().then(String::new).unwrap_or_else(|| {
                content::html_to_text(self.content.as_deref().unwrap_or_default())
            });
        let fresh = moderation::record_report(
            &data.pool,
            self.id.as_str(),
            reporter,
            &handle,
            &objects,
            &comment,
        )
        .await?;
        if fresh {
            tracing::warn!(
                "federation report from {handle} about {} object(s)",
                self.object.len()
            );
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
            InboundActivity::Announce(a) => a.id(),
            InboundActivity::Delete(a) => a.id(),
            InboundActivity::Update(a) => a.id(),
            InboundActivity::Flag(a) => a.id(),
            InboundActivity::Other(a) => a.id(),
        }
    }

    fn actor(&self) -> &Url {
        match self {
            InboundActivity::Follow(a) => a.actor(),
            InboundActivity::Undo(a) => a.actor(),
            InboundActivity::Create(a) => a.actor(),
            InboundActivity::Accept(a) => a.actor(),
            InboundActivity::Announce(a) => a.actor(),
            InboundActivity::Delete(a) => a.actor(),
            InboundActivity::Update(a) => a.actor(),
            InboundActivity::Flag(a) => a.actor(),
            InboundActivity::Other(a) => a.actor(),
        }
    }

    async fn verify(&self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            InboundActivity::Follow(a) => a.verify(data).await,
            InboundActivity::Undo(a) => a.verify(data).await,
            InboundActivity::Create(a) => a.verify(data).await,
            InboundActivity::Accept(a) => a.verify(data).await,
            InboundActivity::Announce(a) => a.verify(data).await,
            InboundActivity::Delete(a) => a.verify(data).await,
            InboundActivity::Update(a) => a.verify(data).await,
            InboundActivity::Flag(a) => a.verify(data).await,
            InboundActivity::Other(a) => a.verify(data).await,
        }
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            InboundActivity::Follow(a) => a.receive(data).await,
            InboundActivity::Undo(a) => a.receive(data).await,
            InboundActivity::Create(a) => a.receive(data).await,
            InboundActivity::Accept(a) => a.receive(data).await,
            InboundActivity::Announce(a) => a.receive(data).await,
            InboundActivity::Delete(a) => a.receive(data).await,
            InboundActivity::Update(a) => a.receive(data).await,
            InboundActivity::Flag(a) => a.receive(data).await,
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

/// Send a private message to a remote fediverse account.
///
/// **Not private.** Fediverse DMs are plaintext on every server they touch; this
/// path exists only behind the `[federation] allow_remote_dms` opt-in, and the
/// compose UI labels it. Resolves the recipient over WebFinger, records a local
/// copy, and queues a Mastodon-compatible direct `Create{Note}` (addressed to
/// the actor, with a matching `Mention`). Returns the resolved recipient handle.
pub async fn send_remote_dm(
    pool: &SqlitePool,
    fed: &Federation,
    from: &crate::db::models::User,
    handle: &str,
    subject: &str,
    body: &str,
    limits: &crate::config::Limits,
) -> anyhow::Result<String> {
    use activitypub_federation::fetch::webfinger::webfinger_resolve_actor;
    anyhow::ensure!(
        fed.enabled && fed.allow_remote_dms,
        "remote mail is disabled on this board"
    );
    let origin = Origin::from_config(fed)?;
    let (user, domain) = split_handle(handle)
        .ok_or_else(|| anyhow::anyhow!("{handle:?} is not a user@host handle"))?;
    let config = build_config(pool.clone(), origin.clone(), fed, &Default::default()).await?;
    let data = config.to_request_data();

    // Sign as the sender; mint their actor if this is their first federated act.
    let keys = ensure_person_keys(pool, &origin, from).await?;

    let recipient: FedActor =
        webfinger_resolve_actor::<AppData, FedActor>(&format!("{user}@{domain}"), &data)
            .await
            .map_err(|e| anyhow::anyhow!("resolving {handle}: {}", e.0))?;
    let recipient_uri = recipient.ap_id.inner().to_string();
    // The resolved actor was persisted; load its row for the local mail record.
    let to_row = crate::services::auth::find_user(pool, &recipient.username)
        .await?
        .ok_or_else(|| anyhow::anyhow!("resolved actor {handle} was not stored"))?;

    // Local record first — its id mints the message's permanent URI.
    let mail_id =
        crate::services::mail::send_remote(pool, from, &to_row, subject, body, limits).await?;

    let create = crate::services::federation::objects::direct_message(
        &origin.dm(mail_id),
        &keys.actor_uri,
        &recipient_uri,
        &recipient.username,
        subject,
        body,
        now_unix(),
    );
    let activity = serde_json::to_string(&create)?;
    queue::enqueue(
        pool,
        &keys.actor_uri,
        recipient.inbox.as_str(),
        &activity,
        Some(&create.id),
    )
    .await?;
    Ok(recipient.username)
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
    let config = build_config(pool.clone(), origin.clone(), fed, &Default::default()).await?;
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
    let config = build_config(pool.clone(), origin.clone(), fed, &Default::default()).await?;
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
    limits: &crate::config::Limits,
) -> anyhow::Result<FederationConfig<AppData>> {
    let origin_host = origin.host().to_string();
    let verifier = AllowlistVerifier {
        pool: pool.clone(),
        origin_host,
        allowlist_only: fed.allowlist_only,
    };
    let app_data = AppData {
        pool,
        origin,
        allow_remote_dms: fed.allow_remote_dms,
        limits: limits.clone(),
    };
    let config = FederationConfig::builder()
        .domain(app_data.origin.host().to_string())
        .app_data(app_data)
        .url_verifier(Box::new(verifier))
        .debug(fed.debug_insecure)
        .build()
        .await?;
    Ok(config)
}
