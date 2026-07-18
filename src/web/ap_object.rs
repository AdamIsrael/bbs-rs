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
use activitypub_federation::kinds::actor::PersonType;
use activitypub_federation::protocol::public_key::PublicKey;
use activitypub_federation::protocol::verification::verify_domains_match;
use activitypub_federation::traits::{Activity, Actor, Object};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use url::Url;

use crate::config::Federation;
use crate::services::federation::{Origin, policy};
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

/// Every activity we're willing to receive — which for this phase is *any*
/// well-formed one. [`receive_activity`](activitypub_federation::axum::inbox::receive_activity)
/// still fetches the signing actor and verifies the HTTP signature before this
/// runs; we just don't *act* on the activity yet. Follow/Accept (Slice B) and
/// remote statuses (Slice C) fill in [`Activity::receive`] by narrowing this
/// into a typed enum.
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
        // The signature is already verified by the time we get here. Acting on
        // the activity is deferred; log it so the wiring is observable.
        tracing::info!(
            "inbound {} from {} — signature verified, accepted (handling lands in #109 Slice B/C)",
            self.kind,
            self.actor.inner()
        );
        Ok(())
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
