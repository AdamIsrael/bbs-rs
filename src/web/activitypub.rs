//! ActivityPub HTTP surface (epic #113): WebFinger discovery, actor documents,
//! nodeinfo (#107), and statuses + outboxes (#108).
//!
//! **Read-only and unauthenticated.** There is no inbox yet (#109), so nothing
//! here accepts input beyond a lookup. That's deliberate: publishing is the
//! lowest-risk way to get URI minting, content negotiation, and wire shapes
//! right before opening a door.
//!
//! It also bounds what this phase can claim. Being *followed* requires an inbox
//! — Mastodon POSTs a `Follow` and expects an `Accept` — so until #109 a user
//! here is **discoverable and fetchable**, not followable.
//!
//! Everything is gated on `[federation] enabled` **and** a validated origin, so
//! a misconfigured board serves 404 rather than minting URIs it's stuck with.

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::services::federation::{self, Origin};

use super::WebState;

/// The content type ActivityPub actually wants. `axum::Json` hardcodes
/// `application/json`, which some implementations reject, so responses go out
/// through this instead.
const AP_CONTENT_TYPE: &str = "application/activity+json";

/// WebFinger's own media type (RFC 7033).
const JRD_CONTENT_TYPE: &str = "application/jrd+json";

/// A JSON response carrying an explicit ActivityPub/JRD content type.
struct ApJson<T>(&'static str, T);

impl<T: Serialize> IntoResponse for ApJson<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.1) {
            Ok(body) => ([(header::CONTENT_TYPE, self.0)], body).into_response(),
            Err(e) => {
                tracing::error!("serializing ActivityPub response: {e}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Resolve the federation origin, or `None` when federation is off or the
/// origin doesn't validate. Callers turn `None` into 404: an instance that
/// can't federate correctly should look like it doesn't federate at all.
fn origin(state: &WebState) -> Option<Origin> {
    let config = state.config.load();
    let fed = &config.federation;
    if !fed.enabled {
        return None;
    }
    match Origin::from_config(fed) {
        Ok(o) => Some(o),
        Err(e) => {
            // Startup validates this too; if we're here the config was reloaded
            // into something invalid, and serving actors from it would mint
            // permanent garbage.
            tracing::warn!("federation is enabled but the origin is invalid: {e:#}");
            None
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WebfingerQuery {
    resource: String,
}

/// `GET /.well-known/webfinger?resource=acct:alice@bbs.example.com`
///
/// The entry point for `@alice@bbs.example.com` resolving anywhere in the
/// fediverse: it maps a handle to an actor URI. Mastodon verifies that the
/// returned actor's own id agrees with the domain queried here, so the
/// `subject` and the `href` must both be built from the same validated origin.
pub async fn webfinger(
    State(state): State<WebState>,
    Query(query): Query<WebfingerQuery>,
) -> Response {
    let Some(origin) = origin(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // `resource` is `acct:user@host`. The host must be *ours* — otherwise we'd
    // happily answer for domains we don't serve.
    let Some((username, domain)) = federation::split_handle(&query.resource) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !domain.eq_ignore_ascii_case(origin.host()) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let user = match federation::find_local_actor(&state.pool, username).await {
        Ok(Some(u)) => u,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("webfinger lookup for {username:?}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let actor_url = match url::Url::parse(&origin.person(&user.username)) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("building actor url: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let response = activitypub_federation::fetch::webfinger::build_webfinger_response(
        origin.acct(&user.username),
        actor_url,
    );
    ApJson(JRD_CONTENT_TYPE, response).into_response()
}

/// `GET /.well-known/nodeinfo` — the discovery document pointing at the real
/// one. Fediverse crawlers and relays expect this before anything else.
pub async fn nodeinfo_index(State(state): State<WebState>) -> Response {
    let Some(origin) = origin(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let doc = serde_json::json!({
        "links": [{
            "rel": "http://nodeinfo.diaspora.software/ns/schema/2.1",
            "href": format!("{}/nodeinfo/2.1", origin.as_str()),
        }],
    });
    ApJson("application/json", doc).into_response()
}

/// `GET /nodeinfo/2.1` — what this instance is and roughly how big.
///
/// `usersTotal` counts local members only; discovered remote actors live in the
/// same table but aren't ours to claim.
pub async fn nodeinfo(State(state): State<WebState>) -> Response {
    if origin(&state).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let stats = crate::services::stats::gather(&state.pool, 1).await.ok();
    let (users, posts) = stats
        .map(|s| (s.total_users, s.total_posts))
        .unwrap_or((0, 0));
    let doc = serde_json::json!({
        "version": "2.1",
        "software": {
            "name": "bbs-rs",
            "version": env!("CARGO_PKG_VERSION"),
            "repository": env!("CARGO_PKG_REPOSITORY"),
        },
        "protocols": ["activitypub"],
        "services": { "inbound": [], "outbound": [] },
        // Registration is in-BBS (from the guest session), not over HTTP.
        "openRegistrations": false,
        "usage": {
            "users": { "total": users },
            "localPosts": posts,
        },
        "metadata": {},
    });
    ApJson("application/json", doc).into_response()
}

/// An ActivityPub `Person`, as served at `/u/{username}`.
///
/// Hand-rolled rather than derived from the crate's `Object` trait: phase 1
/// only publishes actors, and the trait plumbing (`from_json`, `verify`,
/// federation `Data<T>`) earns its keep once we're sending and receiving
/// activities (#108/#109).
// ActivityStreams is camelCase on the wire (`preferredUsername`,
// `publicKeyPem`, `sharedInbox`). Getting this wrong doesn't error — it just
// silently fails to interop.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(rename = "@context")]
    context: Vec<String>,
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    preferred_username: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    summary: String,
    inbox: String,
    outbox: String,
    followers: String,
    endpoints: Endpoints,
    public_key: PublicKeyJson,
    url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Endpoints {
    shared_inbox: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicKeyJson {
    id: String,
    owner: String,
    public_key_pem: String,
}

/// An `OrderedCollection` of activities — the outbox.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderedCollection {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    total_items: i64,
    ordered_items: Vec<federation::objects::CreateNote>,
}

/// `GET /s/{id}` — a status as a `Note`.
pub async fn status(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let Some(origin) = origin(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(Some(o)) = crate::services::oneliners::get(&state.pool, id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // A status by a non-federatable author (guest) isn't published.
    match federation::find_local_actor(&state.pool, &o.author_name).await {
        Ok(Some(_)) => {}
        _ => return StatusCode::NOT_FOUND.into_response(),
    }
    match federation::objects::note_for(&state.pool, &origin, &o).await {
        Ok(note) => ApJson(AP_CONTENT_TYPE, note).into_response(),
        Err(e) => {
            tracing::error!("building note {id}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// How many statuses an outbox page carries. Bounded so a long-lived wall
/// can't produce an unbounded response — the wall no longer auto-trims.
const OUTBOX_LIMIT: i64 = 40;

/// `GET /u/{username}/outbox` — the user's statuses as `Create{Note}`.
///
/// A single collection rather than a paged one: with `OUTBOX_LIMIT` recent
/// items this stays small, and paging earns its keep once there's a consumer
/// that walks it.
pub async fn outbox(State(state): State<WebState>, Path(username): Path<String>) -> Response {
    let Some(origin) = origin(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let user = match federation::find_local_actor(&state.pool, &username).await {
        Ok(Some(u)) => u,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("outbox lookup for {username:?}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let (rows, total) = match tokio::try_join!(
        crate::services::oneliners::by_author(&state.pool, user.id, OUTBOX_LIMIT),
        crate::services::oneliners::count_by_author(&state.pool, user.id),
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("loading outbox for {username:?}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut items = Vec::with_capacity(rows.len());
    for o in &rows {
        match federation::objects::create_for(&state.pool, &origin, o).await {
            Ok(create) => items.push(create),
            Err(e) => {
                tracing::error!("building note {}: {e}", o.id);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }

    let collection = OrderedCollection {
        context: "https://www.w3.org/ns/activitystreams",
        kind: "OrderedCollection",
        id: origin.person_outbox(&user.username),
        total_items: total,
        ordered_items: items,
    };
    ApJson(AP_CONTENT_TYPE, collection).into_response()
}

/// `GET /u/{username}` — the actor document.
///
/// Minting happens here, lazily: the first fetch of an actor generates its
/// keypair. That keeps a board that never federates free of private keys.
pub async fn person(State(state): State<WebState>, Path(username): Path<String>) -> Response {
    let Some(origin) = origin(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let user = match federation::find_local_actor(&state.pool, &username).await {
        Ok(Some(u)) => u,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("actor lookup for {username:?}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let keys = match federation::ensure_person_keys(&state.pool, &origin, &user).await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("minting actor for {username:?}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Profile fields are optional decoration — a failure here shouldn't stop us
    // serving the actor, which is what federation actually depends on.
    let (name, summary) = crate::services::profiles::get_profile(&state.pool, user.id)
        .await
        .map(|p| (p.real_name, p.tagline))
        .unwrap_or_default();

    let doc = Person {
        // Mastodon needs the security context for `publicKey` to be understood.
        context: vec![
            "https://www.w3.org/ns/activitystreams".into(),
            "https://w3id.org/security/v1".into(),
        ],
        kind: "Person",
        // `preferredUsername` must match the acct: handle WebFinger resolves.
        preferred_username: user.username.clone(),
        name,
        summary,
        inbox: origin.person_inbox(&user.username),
        outbox: origin.person_outbox(&user.username),
        followers: origin.person_followers(&user.username),
        endpoints: Endpoints {
            shared_inbox: origin.shared_inbox(),
        },
        public_key: PublicKeyJson {
            // The keyId peers cite when verifying our HTTP signatures.
            id: format!("{}#main-key", keys.actor_uri),
            owner: keys.actor_uri.clone(),
            public_key_pem: keys.public_key,
        },
        url: keys.actor_uri.clone(),
        id: keys.actor_uri,
    };
    ApJson(AP_CONTENT_TYPE, doc).into_response()
}
