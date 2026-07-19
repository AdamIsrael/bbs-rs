//! ActivityPub foundation checks (epic #113, phase #107).
//!
//! The load-bearing test here is `fts_survives_the_activitypub_migration`. The
//! whole additive-schema approach in `0013` rests on one assumption: adding
//! columns to `messages` does not disturb the `messages_fts` FTS5
//! external-content index, because the 0012 triggers name `new.id`/
//! `new.subject`/`new.body` explicitly. If that were wrong we'd be forced into
//! a full table rebuild (dropping and recreating the vtable + all three
//! triggers), which is exactly what the design avoids — so this is pinned.

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

use bbs_rs::services::{self, auth, boards};

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    services::seed(&pool, &Default::default())
        .await
        .expect("seed");
    pool
}

/// Search must still work after 0013 adds columns to `messages` — for rows
/// written both before and after the migration's columns existed.
#[tokio::test]
async fn fts_survives_the_activitypub_migration() {
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Xyzzy plugh",
        "a colossal cave body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // The insert trigger indexed it despite the new columns.
    let hits = bbs_rs::services::search::search_messages(&pool, "user", "xyzzy", 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "FTS should find the post after 0013");
    assert_eq!(hits[0].subject, "Xyzzy plugh");

    // Body terms are indexed too (proves both indexed columns survive).
    let hits = bbs_rs::services::search::search_messages(&pool, "user", "colossal", 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "FTS should match on body after 0013");

    // Writing the *new* AP column must not corrupt the index: the update
    // trigger deletes the old row by (subject, body) and re-inserts, so a
    // mismatch would silently break search.
    sqlx::query("UPDATE messages SET ap_id = ? WHERE id = ?")
        .bind("https://bbs.example.com/post/1")
        .bind(hits[0].id)
        .execute(&pool)
        .await
        .unwrap();

    let hits = bbs_rs::services::search::search_messages(&pool, "user", "xyzzy", 10)
        .await
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "FTS must survive an UPDATE that touches only an AP column"
    );
}

/// 0013 is additive: every pre-existing row keeps working, and the new columns
/// default to a sane local-actor shape.
#[tokio::test]
async fn existing_rows_default_to_local_actors() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    let (domain, is_remote, actor): (String, i64, Option<String>) =
        sqlx::query_as("SELECT domain, is_remote, actor_uri FROM users WHERE username = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(domain, "", "local users have no domain");
    assert_eq!(is_remote, 0, "registered users are local");
    assert!(actor.is_none(), "actor_uri is minted lazily, not at signup");
}

/// Insert a shadow actor the way a federated import will: a fully-qualified
/// handle, an unusable password sentinel, and `is_remote = 1`.
async fn insert_remote_actor(pool: &SqlitePool, handle: &str, domain: &str) {
    sqlx::query(
        "INSERT INTO users (username, password_hash, role, created_at, domain, is_remote, \
         actor_uri, inbox_url) VALUES (?, '!', 'user', 0, ?, 1, ?, ?)",
    )
    .bind(handle)
    .bind(domain)
    .bind(format!(
        "https://{domain}/users/{}",
        handle.split('@').next().unwrap()
    ))
    .bind(format!(
        "https://{domain}/users/{}/inbox",
        handle.split('@').next().unwrap()
    ))
    .execute(pool)
    .await
    .unwrap();
}

/// A discovered remote actor is not an account: it must never authenticate,
/// regardless of what is sent as a password.
#[tokio::test]
async fn remote_actors_cannot_log_in() {
    let pool = setup().await;
    insert_remote_actor(&pool, "alice@remote.social", "remote.social").await;

    for pw in ["", "!", "pw", "password"] {
        let got = auth::verify_login(&pool, "alice@remote.social", pw)
            .await
            .unwrap();
        assert!(got.is_none(), "remote actor logged in with {pw:?}");
    }

    // The full login path (bans + audit) agrees.
    let got = auth::attempt_login(&pool, "alice@remote.social", "!", None)
        .await
        .unwrap();
    assert!(got.is_none(), "attempt_login must reject remote actors");

    // And the model says so directly.
    let user = auth::find_user(&pool, "alice@remote.social")
        .await
        .unwrap()
        .unwrap();
    assert!(user.is_remote);
    assert!(!user.can_log_in());
}

/// Remote actors share the `users` table, so every place that lists or
/// addresses local members must exclude them.
#[tokio::test]
async fn remote_actors_stay_out_of_local_surfaces() {
    use bbs_rs::services::{admin, mail, stats};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    insert_remote_actor(&pool, "eve@remote.social", "remote.social").await;

    // Admin user list: local only.
    let listed = admin::list_users(&pool).await.unwrap();
    assert!(
        listed.iter().all(|u| !u.is_remote),
        "admin list must not include remote actors"
    );
    assert!(!listed.iter().any(|u| u.username.contains('@')));

    // Stats membership count: local only (guest + alice), not the actor.
    let s = stats::gather(&pool, 10).await.unwrap();
    assert_eq!(
        s.total_users, 2,
        "remote actors must not inflate membership"
    );

    // Mail can't address a remote actor — fediverse DMs are not private, so
    // that stays a deliberate opt-in (#110), not an accident.
    assert!(matches!(
        mail::send_mail(
            &pool,
            &alice,
            "eve@remote.social",
            "hi",
            "body",
            &Default::default()
        )
        .await,
        Err(bbs_rs::error::AppError::RecipientNotFound)
    ));
}

/// Minting an actor is lazy and **idempotent**. Re-minting would hand the same
/// user a new URI and a new keypair, orphaning every remote follow and breaking
/// signature verification for everything already delivered — so a second call
/// must return the first result byte-for-byte.
#[tokio::test]
async fn person_keys_are_minted_once_and_reused() {
    use bbs_rs::services::federation::{Origin, ensure_person_keys};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    let first = ensure_person_keys(&pool, &origin, &alice).await.unwrap();
    assert_eq!(first.actor_uri, "https://bbs.example.com/u/alice");
    assert!(first.private_key.contains("BEGIN PRIVATE KEY"));
    assert!(first.public_key.contains("BEGIN PUBLIC KEY"));

    let second = ensure_person_keys(&pool, &origin, &alice).await.unwrap();
    assert_eq!(first.actor_uri, second.actor_uri);
    assert_eq!(
        first.private_key, second.private_key,
        "the keypair must never be regenerated"
    );

    // Even if the configured origin later changes, an existing actor keeps its
    // URI — the old one is already out in the world.
    let moved = Origin::new("https://elsewhere.example.net");
    let third = ensure_person_keys(&pool, &moved, &alice).await.unwrap();
    assert_eq!(
        third.actor_uri, "https://bbs.example.com/u/alice",
        "a minted actor_uri is permanent, even if the origin config moves"
    );

    // The inbox was recorded alongside it.
    let inbox: Option<String> =
        sqlx::query_scalar("SELECT inbox_url FROM users WHERE username = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        inbox.as_deref(),
        Some("https://bbs.example.com/u/alice/inbox")
    );
}

/// Only real local members get actors: `guest` is shared (one keypair for many
/// humans), and remote rows belong to other servers.
#[tokio::test]
async fn guest_and_remote_actors_are_not_federatable() {
    use bbs_rs::services::federation::{Origin, ensure_person_keys, find_local_actor};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    insert_remote_actor(&pool, "eve@remote.social", "remote.social").await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // WebFinger resolves only local, non-guest accounts.
    assert!(find_local_actor(&pool, "alice").await.unwrap().is_some());
    assert!(
        find_local_actor(&pool, "guest").await.unwrap().is_none(),
        "the shared guest account must not be federatable"
    );
    assert!(
        find_local_actor(&pool, "eve@remote.social")
            .await
            .unwrap()
            .is_none(),
        "a remote actor is not one of ours to publish"
    );
    assert!(find_local_actor(&pool, "nobody").await.unwrap().is_none());

    // We never mint keys for someone else's actor.
    let eve = auth::find_user(&pool, "eve@remote.social")
        .await
        .unwrap()
        .unwrap();
    assert!(ensure_person_keys(&pool, &origin, &eve).await.is_err());
}

/// Serve the AP endpoints on an ephemeral port with the given federation
/// config, returning the base URL.
async fn serve_ap(pool: SqlitePool, fed: bbs_rs::config::Federation) -> String {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use arc_swap::ArcSwap;
    use bbs_rs::config::Settings;
    use bbs_rs::services::presence::Presence;

    let settings = Settings {
        federation: fed.clone(),
        ..Default::default()
    };
    let mut state = bbs_rs::web::WebState::new(
        pool.clone(),
        Arc::new(ArcSwap::from_pointee(settings)),
        Presence::new(),
        Arc::new(AtomicUsize::new(0)),
    );
    // Attach the inbound federation machinery, mirroring lib::serve, so the
    // inbox routes exist and receive_activity can verify signatures. An invalid
    // origin (a fail-closed case) simply doesn't attach — the server still runs
    // and its handlers 404, which is what production does too.
    if fed.enabled
        && let Ok(origin) = bbs_rs::services::federation::Origin::from_config(&fed)
        && let Ok(config) =
            bbs_rs::web::ap_object::build_config(pool, origin, &fed, &Default::default()).await
    {
        state = state.with_federation(config);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = bbs_rs::web::serve(listener, state).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    format!("http://{addr}")
}

fn enabled_fed() -> bbs_rs::config::Federation {
    bbs_rs::config::Federation {
        enabled: true,
        origin: "https://bbs.example.com".into(),
        ..Default::default()
    }
}

/// WebFinger is how `@alice@bbs.example.com` resolves anywhere in the
/// fediverse. Mastodon checks that the actor it fetches agrees with the domain
/// it asked about, so subject/href must both come from the validated origin.
#[tokio::test]
async fn webfinger_resolves_a_local_actor() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let base = serve_ap(pool, enabled_fed()).await;

    let res = reqwest::get(format!(
        "{base}/.well-known/webfinger?resource=acct:alice@bbs.example.com"
    ))
    .await
    .unwrap();
    assert_eq!(res.status(), 200);
    // RFC 7033 media type, not application/json.
    assert_eq!(
        res.headers()["content-type"],
        "application/jrd+json",
        "webfinger must use the JRD content type"
    );

    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["subject"], "acct:alice@bbs.example.com");
    let self_link = body["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["rel"] == "self")
        .expect("a rel=self link is what points at the actor");
    assert_eq!(self_link["type"], "application/activity+json");
    assert_eq!(self_link["href"], "https://bbs.example.com/u/alice");
}

/// We must only answer for our own domain, and only for real local members.
#[tokio::test]
async fn webfinger_refuses_what_isnt_ours() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    insert_remote_actor(&pool, "eve@remote.social", "remote.social").await;
    let base = serve_ap(pool, enabled_fed()).await;

    let get = |q: String| {
        let base = base.clone();
        async move {
            reqwest::get(format!("{base}/.well-known/webfinger?resource={q}"))
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };

    // A domain we don't serve.
    assert_eq!(get("acct:alice@elsewhere.example".into()).await, 404);
    // The shared guest account is not federatable.
    assert_eq!(get("acct:guest@bbs.example.com".into()).await, 404);
    // A remote actor is not ours to publish.
    assert_eq!(get("acct:eve@remote.social".into()).await, 404);
    // Nonexistent, and malformed.
    assert_eq!(get("acct:nobody@bbs.example.com".into()).await, 404);
    assert_eq!(get("nonsense".into()).await, 400);
}

/// The actor document is what a remote server fetches after WebFinger. Field
/// names are camelCase on the wire — getting that wrong doesn't error, it just
/// silently fails to interop.
#[tokio::test]
async fn person_actor_document_is_shaped_for_interop() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let base = serve_ap(pool, enabled_fed()).await;

    let res = reqwest::get(format!("{base}/u/alice")).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["content-type"],
        "application/activity+json",
        "actors must not be served as plain application/json"
    );

    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["type"], "Person");
    assert_eq!(b["id"], "https://bbs.example.com/u/alice");
    // The handle WebFinger resolves must match preferredUsername.
    assert_eq!(b["preferredUsername"], "alice");
    assert_eq!(b["inbox"], "https://bbs.example.com/u/alice/inbox");
    assert_eq!(b["outbox"], "https://bbs.example.com/u/alice/outbox");
    assert_eq!(b["followers"], "https://bbs.example.com/u/alice/followers");
    assert_eq!(
        b["endpoints"]["sharedInbox"],
        "https://bbs.example.com/inbox"
    );
    // The security context is required for publicKey to be understood.
    assert!(
        b["@context"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "https://w3id.org/security/v1"),
        "the security context must be present for publicKey"
    );
    // keyId is what peers cite when verifying our HTTP signatures.
    assert_eq!(
        b["publicKey"]["id"],
        "https://bbs.example.com/u/alice#main-key"
    );
    assert_eq!(b["publicKey"]["owner"], "https://bbs.example.com/u/alice");
    assert!(
        b["publicKey"]["publicKeyPem"]
            .as_str()
            .unwrap()
            .contains("BEGIN PUBLIC KEY"),
        "publicKeyPem must carry a real PEM key"
    );

    // Guests and unknown names aren't actors.
    for name in ["guest", "nobody"] {
        let status = reqwest::get(format!("{base}/u/{name}"))
            .await
            .unwrap()
            .status();
        assert_eq!(status, 404, "/u/{name} must not resolve");
    }
}

/// A board that isn't federating — or is misconfigured — must not expose an AP
/// surface at all, rather than minting URIs it would be stuck with.
#[tokio::test]
async fn ap_endpoints_are_closed_unless_configured() {
    use bbs_rs::config::Federation;

    // Federation off (the default).
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let base = serve_ap(pool, Federation::default()).await;
    assert_eq!(
        reqwest::get(format!("{base}/u/alice"))
            .await
            .unwrap()
            .status(),
        404
    );

    // Enabled but with an origin that would poison every URI it minted.
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let base = serve_ap(
        pool.clone(),
        Federation {
            enabled: true,
            origin: "https://localhost:8088".into(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        reqwest::get(format!("{base}/u/alice"))
            .await
            .unwrap()
            .status(),
        404,
        "an invalid origin must fail closed"
    );
    // ...and crucially, nothing was minted on the way to that 404.
    let actor: Option<String> =
        sqlx::query_scalar("SELECT actor_uri FROM users WHERE username = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        actor.is_none(),
        "a rejected origin must never mint a permanent actor_uri"
    );
}

/// Post some statuses (oneliners) as `user`.
async fn post_statuses(pool: &SqlitePool, user: &bbs_rs::db::models::User, bodies: &[&str]) {
    use bbs_rs::config::Limits;
    let limits = Limits {
        window_secs: 0, // rate limiting off for the fixture
        ..Default::default()
    };
    for b in bodies {
        bbs_rs::services::oneliners::add(pool, user, b, &limits, &Default::default())
            .await
            .unwrap();
    }
}

/// A status is a `Note`: plain text becomes HTML, addressed to the **full**
/// Public URI (the `as:Public` CURIE is a known interop bug), and its ap_id is
/// minted on first serve.
#[tokio::test]
async fn status_is_served_as_a_note() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    post_statuses(&pool, &alice, &["hello <fediverse> & \"friends\""]).await;
    let base = serve_ap(pool.clone(), enabled_fed()).await;

    let res = reqwest::get(format!("{base}/s/1")).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "application/activity+json");

    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["type"], "Note");
    assert_eq!(b["id"], "https://bbs.example.com/s/1");
    assert_eq!(b["attributedTo"], "https://bbs.example.com/u/alice");
    assert_eq!(
        b["to"][0], "https://www.w3.org/ns/activitystreams#Public",
        "must emit the full Public URI, not the as:Public CURIE"
    );
    assert_eq!(b["cc"][0], "https://bbs.example.com/u/alice/followers");
    // AP content is HTML, so a body must not be able to inject markup.
    assert_eq!(
        b["content"], "<p>hello &lt;fediverse&gt; &amp; &quot;friends&quot;</p>",
        "status bodies must be escaped into their HTML wrapper"
    );
    // RFC 3339, which is what `published` requires.
    let published = b["published"].as_str().unwrap();
    assert!(
        published.ends_with('Z') && published.contains('T') && published.len() == 20,
        "published must be RFC 3339 UTC, got {published:?}"
    );

    // The ap_id was persisted on that first fetch — it's permanent now.
    let ap_id: Option<String> = sqlx::query_scalar("SELECT ap_id FROM oneliners WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ap_id.as_deref(), Some("https://bbs.example.com/s/1"));

    assert_eq!(
        reqwest::get(format!("{base}/s/999"))
            .await
            .unwrap()
            .status(),
        404
    );
}

/// The outbox is the user's statuses as `Create{Note}` — what a remote server
/// fetches to see what someone has posted.
#[tokio::test]
async fn outbox_lists_create_note_activities() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    post_statuses(&pool, &alice, &["first", "second"]).await;
    post_statuses(&pool, &bob, &["not alice's"]).await;
    let base = serve_ap(pool, enabled_fed()).await;

    let res = reqwest::get(format!("{base}/u/alice/outbox"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "application/activity+json");

    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["type"], "OrderedCollection");
    assert_eq!(b["id"], "https://bbs.example.com/u/alice/outbox");
    assert_eq!(b["totalItems"], 2, "only alice's own statuses");

    let items = b["orderedItems"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest first.
    assert_eq!(items[0]["type"], "Create");
    assert_eq!(items[0]["actor"], "https://bbs.example.com/u/alice");
    assert_eq!(items[0]["object"]["content"], "<p>second</p>");
    assert_eq!(items[1]["object"]["content"], "<p>first</p>");
    // The Create and its Note are separate identities.
    assert_eq!(items[0]["object"]["id"], "https://bbs.example.com/s/2");
    assert_eq!(items[0]["id"], "https://bbs.example.com/s/2/activity");

    // Guests and unknowns have no outbox.
    for name in ["guest", "nobody"] {
        assert_eq!(
            reqwest::get(format!("{base}/u/{name}/outbox"))
                .await
                .unwrap()
                .status(),
            404
        );
    }
}

/// nodeinfo is how crawlers and relays identify the instance.
#[tokio::test]
async fn nodeinfo_describes_the_instance() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    insert_remote_actor(&pool, "eve@remote.social", "remote.social").await;
    post_statuses(&pool, &alice, &["hi"]).await;
    let base = serve_ap(pool, enabled_fed()).await;

    // Discovery document points at the real one.
    let b: serde_json::Value = reqwest::get(format!("{base}/.well-known/nodeinfo"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        b["links"][0]["href"], "https://bbs.example.com/nodeinfo/2.1",
        "discovery must point at our origin, not the request host"
    );

    let b: serde_json::Value = reqwest::get(format!("{base}/nodeinfo/2.1"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(b["version"], "2.1");
    assert_eq!(b["software"]["name"], "bbs-rs");
    assert_eq!(b["protocols"][0], "activitypub");
    // Registration happens in the BBS, not over HTTP.
    assert_eq!(b["openRegistrations"], false);
    // Local members only — the remote actor is not ours to count.
    assert_eq!(b["usage"]["users"]["total"], 2);
}

/// The delivery queue is durable because the AP crate's is in-memory: a restart
/// inside its 1min/1hr/2.5day retry window silently drops deliveries.
#[tokio::test]
async fn delivery_queue_backs_off_and_eventually_gives_up() {
    use bbs_rs::services::federation::queue;
    let pool = setup().await;

    let id = queue::enqueue(
        &pool,
        "https://bbs.example.com/u/alice",
        "https://remote.social/users/bob/inbox",
        r#"{"type":"Create"}"#,
        Some("https://bbs.example.com/s/1/activity"),
    )
    .await
    .unwrap();
    assert_eq!(queue::pending(&pool).await.unwrap(), 1);

    // A fresh delivery is due immediately.
    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, id);
    assert_eq!(due[0].attempts, 0);
    assert_eq!(due[0].inbox_url, "https://remote.social/users/bob/inbox");

    // A failure backs it off, so it's no longer due...
    assert!(
        !queue::mark_failed(&pool, id, "connection refused", 10)
            .await
            .unwrap()
    );
    assert!(
        queue::due(&pool, 10).await.unwrap().is_empty(),
        "a backed-off delivery must not be retried immediately"
    );
    assert_eq!(
        queue::pending(&pool).await.unwrap(),
        1,
        "but it's still queued"
    );

    // ...and the backoff grows, patiently. Hammering a struggling peer is how
    // you get defederated.
    assert_eq!(queue::backoff_secs(1), 120);
    assert_eq!(queue::backoff_secs(2), 240);
    assert!(queue::backoff_secs(3) < queue::backoff_secs(4));
    assert_eq!(
        queue::backoff_secs(99),
        60 * 60 * 3,
        "backoff is capped, and huge attempt counts must not overflow"
    );

    // Past max_attempts we give up: a peer can vanish permanently, and a queue
    // that never forgets grows without bound.
    for _ in 0..10 {
        queue::mark_failed(&pool, id, "gone", 3).await.unwrap();
    }
    assert_eq!(
        queue::pending(&pool).await.unwrap(),
        0,
        "a dead delivery must eventually be dropped"
    );
    // Marking a row that's already gone is harmless.
    assert!(!queue::mark_failed(&pool, id, "gone", 3).await.unwrap());
}

/// Success removes the row — the queue holds outstanding work only.
#[tokio::test]
async fn delivered_activities_leave_the_queue() {
    use bbs_rs::services::federation::queue;
    let pool = setup().await;
    let a = queue::enqueue(&pool, "actor", "https://a.example/inbox", "{}", None)
        .await
        .unwrap();
    // One row per (activity, inbox), so a single dead server can't stall the
    // deliveries bound for everyone else.
    let b = queue::enqueue(&pool, "actor", "https://b.example/inbox", "{}", None)
        .await
        .unwrap();
    assert_eq!(queue::pending(&pool).await.unwrap(), 2);

    queue::mark_delivered(&pool, a).await.unwrap();
    let left = queue::due(&pool, 10).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, b);
}

/// The inbox exists only when federation is on, and it **requires a valid HTTP
/// signature** — an unsigned POST is rejected before any handling. This is the
/// security boundary the whole inbound phase rests on.
#[tokio::test]
async fn inbox_requires_a_signature() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let base = serve_ap(pool, enabled_fed()).await;
    let client = reqwest::Client::new();

    // A well-formed activity, but no HTTP signature.
    let body = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://remote.social/activities/1",
        "type": "Follow",
        "actor": "https://remote.social/users/bob",
        "object": "https://bbs.example.com/u/alice"
    });
    for path in ["/inbox", "/u/alice/inbox"] {
        let res = client
            .post(format!("{base}{path}"))
            .header("content-type", "application/activity+json")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_ne!(
            res.status(),
            200,
            "an unsigned activity must not be accepted at {path}"
        );
    }
}

/// With federation off, there is no inbox at all — a non-federating board must
/// not expose one.
#[tokio::test]
async fn inbox_absent_when_federation_disabled() {
    let pool = setup().await;
    let base = serve_ap(pool, bbs_rs::config::Federation::default()).await;
    let client = reqwest::Client::new();
    for path in ["/inbox", "/u/alice/inbox"] {
        let res = client
            .post(format!("{base}{path}"))
            .json(&serde_json::json!({"type": "Follow"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            404,
            "no inbox when federation is off ({path})"
        );
    }
}

/// The domain policy: allowlist (default) admits only listed domains; blocklist
/// admits all but listed ones. Our own origin is always allowed.
#[tokio::test]
async fn domain_policy_allowlist_and_blocklist() {
    use bbs_rs::services::federation::policy;
    let pool = setup().await;
    let origin = "bbs.example.com";

    // Allowlist mode (the default posture): nothing federates until allowed.
    assert!(
        !policy::domain_allowed(&pool, origin, "remote.social", true)
            .await
            .unwrap()
    );
    // ...except ourselves.
    assert!(
        policy::domain_allowed(&pool, origin, "bbs.example.com", true)
            .await
            .unwrap()
    );
    policy::set(&pool, "friend.example", "allow", "a peer", "suspend")
        .await
        .unwrap();
    assert!(
        policy::domain_allowed(&pool, origin, "friend.example", true)
            .await
            .unwrap()
    );
    assert!(
        policy::domain_allowed(&pool, origin, "FRIEND.EXAMPLE", true)
            .await
            .unwrap(),
        "domain matching is case-insensitive"
    );

    // Blocklist mode: everyone federates except the listed.
    assert!(
        policy::domain_allowed(&pool, origin, "stranger.example", false)
            .await
            .unwrap()
    );
    policy::set(&pool, "spam.example", "block", "spam", "suspend")
        .await
        .unwrap();
    assert!(
        !policy::domain_allowed(&pool, origin, "spam.example", false)
            .await
            .unwrap()
    );

    // Lists and removal round-trip.
    assert_eq!(policy::list(&pool, "allow").await.unwrap().len(), 1);
    assert!(
        policy::unset(&pool, "friend.example", "allow")
            .await
            .unwrap()
    );
    assert!(
        !policy::unset(&pool, "friend.example", "allow")
            .await
            .unwrap()
    );
    assert!(
        !policy::domain_allowed(&pool, origin, "friend.example", true)
            .await
            .unwrap()
    );
}

/// `actor_uri` is globally unique, but NULLs stay distinct — so any number of
/// not-yet-federated local rows can coexist.
#[tokio::test]
async fn actor_uri_is_unique_but_nulls_coexist() {
    let pool = setup().await;
    for name in ["alice", "bob", "carol"] {
        auth::register_user(&pool, name, "pw", &Default::default())
            .await
            .unwrap();
    }
    // Three users + seeded guest, all with NULL actor_uri — no UNIQUE conflict.
    let nulls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE actor_uri IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(nulls, 4);

    sqlx::query("UPDATE users SET actor_uri = ? WHERE username = 'alice'")
        .bind("https://bbs.example.com/u/alice")
        .execute(&pool)
        .await
        .unwrap();

    // The same URI cannot be claimed twice.
    let dup = sqlx::query("UPDATE users SET actor_uri = ? WHERE username = 'bob'")
        .bind("https://bbs.example.com/u/alice")
        .execute(&pool)
        .await;
    assert!(dup.is_err(), "actor_uri must be globally unique");
}

// ---- Slice B: follows, delivery fan-out, inbound Follow/Undo (#109) --------

/// Mint a local actor (registered + keypair) so it can be followed and can
/// sign. Returns the `User`; its actor URI is `{origin}/u/{name}`.
async fn mint_local(pool: &SqlitePool, name: &str) -> bbs_rs::db::models::User {
    use bbs_rs::services::federation::{Origin, ensure_person_keys};
    let origin = Origin::new("https://bbs.example.com");
    let user = auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap();
    ensure_person_keys(pool, &origin, &user).await.unwrap();
    user
}

/// Insert a remote follower complete enough to dereference as an actor (inbox +
/// public key). `shared_inbox` collapses many followers on one server onto a
/// single delivery. Returns the follower's actor URI.
async fn insert_follower(
    pool: &SqlitePool,
    handle: &str,
    domain: &str,
    shared_inbox: Option<&str>,
) -> String {
    let user = handle.split('@').next().unwrap();
    let actor_uri = format!("https://{domain}/users/{user}");
    let inbox = format!("https://{domain}/users/{user}/inbox");
    sqlx::query(
        "INSERT INTO users (username, password_hash, role, created_at, domain, is_remote, \
         actor_uri, inbox_url, shared_inbox_url, public_key) \
         VALUES (?, '!', 'user', 0, ?, 1, ?, ?, ?, 'PUBKEY')",
    )
    .bind(handle)
    .bind(domain)
    .bind(&actor_uri)
    .bind(&inbox)
    .bind(shared_inbox)
    .execute(pool)
    .await
    .unwrap();
    actor_uri
}

/// The follower graph: follows are stored idempotently and unfollowing drops a
/// single edge.
#[tokio::test]
async fn follows_are_stored_idempotently_and_removable() {
    use bbs_rs::services::federation::follows;
    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;

    follows::accept(&pool, &bob, &alice, "https://remote.social/f/1")
        .await
        .unwrap();
    follows::accept(&pool, &carol, &alice, "https://other.example/f/1")
        .await
        .unwrap();
    assert_eq!(follows::count(&pool, &alice).await.unwrap(), 2);

    // A repeat Follow from the same actor doesn't double-count.
    follows::accept(&pool, &bob, &alice, "https://remote.social/f/1b")
        .await
        .unwrap();
    assert_eq!(follows::count(&pool, &alice).await.unwrap(), 2);

    let inboxes = follows::follower_inboxes(&pool, &alice).await.unwrap();
    assert_eq!(inboxes.len(), 2);
    assert!(inboxes.contains(&"https://remote.social/users/bob/inbox".to_string()));

    assert!(follows::remove(&pool, &bob, &alice).await.unwrap());
    assert_eq!(follows::count(&pool, &alice).await.unwrap(), 1);
    assert!(
        !follows::remove(&pool, &bob, &alice).await.unwrap(),
        "removing a follow that's already gone is harmless"
    );
}

/// Many followers behind one shared inbox collapse into a single delivery —
/// that's the whole point of a shared inbox.
#[tokio::test]
async fn a_shared_inbox_collapses_followers_into_one_delivery() {
    use bbs_rs::services::federation::follows;
    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let shared = "https://big.instance/inbox";
    let x = insert_follower(&pool, "x@big.instance", "big.instance", Some(shared)).await;
    let y = insert_follower(&pool, "y@big.instance", "big.instance", Some(shared)).await;
    follows::accept(&pool, &x, &alice, "https://big.instance/f/x")
        .await
        .unwrap();
    follows::accept(&pool, &y, &alice, "https://big.instance/f/y")
        .await
        .unwrap();

    let inboxes = follows::follower_inboxes(&pool, &alice).await.unwrap();
    assert_eq!(
        inboxes,
        vec![shared.to_string()],
        "two followers, one shared inbox, one delivery"
    );
}

/// Posting a status enqueues one `Create{Note}` per distinct follower inbox,
/// signed by the author and carrying the escaped status body.
#[tokio::test]
async fn posting_a_status_fans_out_to_followers() {
    use bbs_rs::services::federation::{Origin, follows, outbound, queue};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    let alice = mint_local(&pool, "alice").await;
    let alice_uri = origin.person("alice");
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;
    follows::accept(&pool, &bob, &alice_uri, "f1")
        .await
        .unwrap();
    follows::accept(&pool, &carol, &alice_uri, "f2")
        .await
        .unwrap();

    post_statuses(&pool, &alice, &["hello <followers>"]).await; // oneliner id 1
    let queued = outbound::deliver_status(&pool, &origin, 1).await.unwrap();
    assert_eq!(queued, 2, "one delivery per follower inbox");

    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(due.len(), 2);
    for d in &due {
        assert_eq!(d.actor_uri, alice_uri, "the author signs the delivery");
        let v: serde_json::Value = serde_json::from_str(&d.activity).unwrap();
        assert_eq!(v["type"], "Create");
        assert_eq!(v["actor"], alice_uri);
        assert_eq!(v["object"]["type"], "Note");
        assert_eq!(
            v["object"]["content"], "<p>hello &lt;followers&gt;</p>",
            "the delivered body is HTML-escaped, same as the published Note"
        );
    }
    let inboxes: std::collections::HashSet<String> =
        due.iter().map(|d| d.inbox_url.clone()).collect();
    assert!(inboxes.contains("https://remote.social/users/bob/inbox"));
    assert!(inboxes.contains("https://other.example/users/carol/inbox"));
}

/// With no remote followers, a post queues nothing — the common case, and not
/// an error.
#[tokio::test]
async fn posting_a_status_with_no_followers_queues_nothing() {
    use bbs_rs::services::federation::{Origin, outbound, queue};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    let alice = mint_local(&pool, "alice").await;
    post_statuses(&pool, &alice, &["into the void"]).await;

    assert_eq!(
        outbound::deliver_status(&pool, &origin, 1).await.unwrap(),
        0
    );
    assert_eq!(queue::pending(&pool).await.unwrap(), 0);
}

/// An inbound `Follow` (whose signature `receive_activity` has already checked)
/// is stored and answered with a queued `Accept` addressed to the follower and
/// signed by the followed local actor. This is what makes a bbs-rs user
/// followable from real Mastodon.
#[tokio::test]
async fn inbound_follow_is_stored_and_accepted() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{Origin, follows, queue};
    use bbs_rs::web::ap_object::{Follow, build_config};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;

    let fed = enabled_fed();
    let origin = Origin::from_config(&fed).unwrap();
    let config = build_config(pool.clone(), origin, &fed, &Default::default())
        .await
        .unwrap();
    let data = config.to_request_data();

    let follow: Follow = serde_json::from_value(serde_json::json!({
        "type": "Follow",
        "id": "https://remote.social/f/1",
        "actor": bob,
        "object": alice_uri,
    }))
    .unwrap();
    follow.receive(&data).await.unwrap();

    assert_eq!(follows::count(&pool, &alice_uri).await.unwrap(), 1);
    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(
        due.len(),
        1,
        "an Accept must be queued back to the follower"
    );
    assert_eq!(due[0].inbox_url, "https://remote.social/users/bob/inbox");
    assert_eq!(due[0].actor_uri, alice_uri, "we sign the Accept as alice");
    let v: serde_json::Value = serde_json::from_str(&due[0].activity).unwrap();
    assert_eq!(v["type"], "Accept");
    assert_eq!(v["actor"], alice_uri);
    assert_eq!(
        v["object"]["type"], "Follow",
        "the Accept echoes the Follow"
    );
    assert_eq!(v["object"]["id"], "https://remote.social/f/1");
}

/// A `Follow` of a non-local actor (e.g. a remote row we happen to know) is
/// ignored — we only accept follows of our own users.
#[tokio::test]
async fn inbound_follow_of_a_remote_actor_is_ignored() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{Origin, follows, queue};
    use bbs_rs::web::ap_object::{Follow, build_config};

    let pool = setup().await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;

    let fed = enabled_fed();
    let origin = Origin::from_config(&fed).unwrap();
    let config = build_config(pool.clone(), origin, &fed, &Default::default())
        .await
        .unwrap();
    let data = config.to_request_data();

    let follow: Follow = serde_json::from_value(serde_json::json!({
        "type": "Follow",
        "id": "https://remote.social/f/9",
        "actor": bob,
        "object": carol,
    }))
    .unwrap();
    follow.receive(&data).await.unwrap();

    assert_eq!(follows::count(&pool, &carol).await.unwrap(), 0);
    assert_eq!(
        queue::pending(&pool).await.unwrap(),
        0,
        "no Accept for a follow that isn't ours"
    );
}

/// An inbound `Undo{Follow}` removes the follow — an unfollow from Mastodon.
#[tokio::test]
async fn inbound_undo_follow_removes_the_follow() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{Origin, follows};
    use bbs_rs::web::ap_object::{Undo, build_config};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    follows::accept(&pool, &bob, &alice_uri, "https://remote.social/f/1")
        .await
        .unwrap();
    assert_eq!(follows::count(&pool, &alice_uri).await.unwrap(), 1);

    let fed = enabled_fed();
    let origin = Origin::from_config(&fed).unwrap();
    let config = build_config(pool.clone(), origin, &fed, &Default::default())
        .await
        .unwrap();
    let data = config.to_request_data();

    let undo: Undo = serde_json::from_value(serde_json::json!({
        "type": "Undo",
        "id": "https://remote.social/u/1",
        "actor": bob,
        "object": {
            "type": "Follow",
            "id": "https://remote.social/f/1",
            "actor": bob,
            "object": alice_uri,
        }
    }))
    .unwrap();
    undo.receive(&data).await.unwrap();

    assert_eq!(
        follows::count(&pool, &alice_uri).await.unwrap(),
        0,
        "the unfollow must drop the edge"
    );
}

// ---- Slice C1: inbound remote statuses → timeline (#109) -------------------

/// Build a federation `Data` for driving inbound activities directly in tests.
async fn fed_data(
    pool: &SqlitePool,
) -> activitypub_federation::config::Data<bbs_rs::web::ap_object::AppData> {
    use bbs_rs::services::federation::Origin;
    let fed = enabled_fed();
    let origin = Origin::from_config(&fed).unwrap();
    let config =
        bbs_rs::web::ap_object::build_config(pool.clone(), origin, &fed, &Default::default())
            .await
            .unwrap();
    config.to_request_data()
}

/// A remote `Create{Note}` from a followed account is degraded to text and
/// cached in the timeline.
#[tokio::test]
async fn inbound_status_from_a_followed_account_lands_in_the_timeline() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, timeline};
    use bbs_rs::web::ap_object::Create;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    // alice follows bob (an outbound, accepted follow).
    follows::accept(&pool, &alice, &bob, "https://bbs.example.com/f/1")
        .await
        .unwrap();

    let data = fed_data(&pool).await;
    let create: Create = serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/activities/1",
        "actor": bob,
        "object": {
            "type": "Note",
            "id": "https://remote.social/notes/1",
            "attributedTo": bob,
            "content": "<p>Hello from <a href=\"https://remote.social/tags/rust\">#rust</a></p>",
            "url": "https://remote.social/@bob/1",
            "published": "2026-07-01T12:00:00Z",
        }
    }))
    .unwrap();
    create.receive(&data).await.unwrap();

    let entries = timeline::recent(&pool, 10).await.unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.ap_id, "https://remote.social/notes/1");
    assert_eq!(e.author_handle, "bob@remote.social");
    assert_eq!(
        e.content, "Hello from #rust (https://remote.social/tags/rust)",
        "HTML is degraded to plain text at ingestion"
    );
    assert_eq!(e.url.as_deref(), Some("https://remote.social/@bob/1"));
    assert_eq!(e.published, 1_782_907_200, "published parsed from RFC 3339");
}

/// A status from an account nobody here follows is dropped — we don't cache the
/// whole fediverse, only what someone asked to see.
#[tokio::test]
async fn inbound_status_from_an_unfollowed_account_is_dropped() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::timeline;
    use bbs_rs::web::ap_object::Create;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    // No follow row: alice does not follow bob.

    let data = fed_data(&pool).await;
    let create: Create = serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/activities/2",
        "actor": bob,
        "object": {
            "type": "Note",
            "id": "https://remote.social/notes/2",
            "attributedTo": bob,
            "content": "<p>spam</p>",
        }
    }))
    .unwrap();
    create.receive(&data).await.unwrap();

    assert_eq!(timeline::count(&pool).await.unwrap(), 0);
}

/// A `Create` whose Note is attributed to someone other than the signer is
/// ignored — a followed account can't inject posts as a third party.
#[tokio::test]
async fn inbound_status_with_a_forged_author_is_ignored() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, timeline};
    use bbs_rs::web::ap_object::Create;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;
    // alice follows bob, but the note claims to be carol's while bob signs it.
    follows::accept(&pool, &alice, &bob, "https://bbs.example.com/f/1")
        .await
        .unwrap();

    let data = fed_data(&pool).await;
    let create: Create = serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/activities/3",
        "actor": bob,
        "object": {
            "type": "Note",
            "id": "https://remote.social/notes/3",
            "attributedTo": carol,
            "content": "<p>impersonation</p>",
        }
    }))
    .unwrap();
    create.receive(&data).await.unwrap();

    assert_eq!(timeline::count(&pool).await.unwrap(), 0);
}

/// The same status delivered twice (a redelivery, or to two of our followers)
/// caches once — the Note's `ap_id` is the key.
#[tokio::test]
async fn a_redelivered_status_is_cached_only_once() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, timeline};
    use bbs_rs::web::ap_object::Create;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    follows::accept(&pool, &alice, &bob, "https://bbs.example.com/f/1")
        .await
        .unwrap();

    let data = fed_data(&pool).await;
    let make = || {
        serde_json::from_value::<Create>(serde_json::json!({
            "type": "Create",
            "id": "https://remote.social/activities/4",
            "actor": bob,
            "object": {
                "type": "Note",
                "id": "https://remote.social/notes/4",
                "attributedTo": bob,
                "content": "<p>once</p>",
            }
        }))
        .unwrap()
    };
    make().receive(&data).await.unwrap();
    make().receive(&data).await.unwrap();

    assert_eq!(timeline::count(&pool).await.unwrap(), 1);
}

// ---- Slice C2: outbound follow lifecycle (#109) ----------------------------

/// A local user's outbound follow is `pending` until the remote answers, then
/// an inbound `Accept` flips it to `accepted`.
#[tokio::test]
async fn an_accept_confirms_a_pending_outbound_follow() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::follows;
    use bbs_rs::web::ap_object::AcceptFollow;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;

    // alice asks to follow bob.
    let follow_id = format!("{alice}#follow/1");
    follows::request(&pool, &alice, &bob, &follow_id)
        .await
        .unwrap();
    assert_eq!(
        follows::following(&pool, &alice).await.unwrap(),
        vec![(bob.clone(), "pending".to_string())]
    );
    // Not yet a source for the timeline — the follow isn't accepted.
    assert!(!follows::is_followed_locally(&pool, &bob).await.unwrap());

    // bob's server accepts, echoing our Follow back.
    let data = fed_data(&pool).await;
    let accept: AcceptFollow = serde_json::from_value(serde_json::json!({
        "type": "Accept",
        "id": "https://remote.social/activities/accept/1",
        "actor": bob,
        "object": {
            "type": "Follow",
            "id": follow_id,
            "actor": alice,
            "object": bob,
        }
    }))
    .unwrap();
    accept.receive(&data).await.unwrap();

    assert_eq!(
        follows::following(&pool, &alice).await.unwrap(),
        vec![(bob.clone(), "accepted".to_string())]
    );
    // Now a followed account: its statuses would be cached.
    assert!(follows::is_followed_locally(&pool, &bob).await.unwrap());
}

/// An `Accept` signed by someone other than the account that was followed is
/// ignored — a server can only accept follows addressed to it.
#[tokio::test]
async fn an_accept_from_the_wrong_signer_is_ignored() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::follows;
    use bbs_rs::web::ap_object::AcceptFollow;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;

    let follow_id = format!("{alice}#follow/1");
    follows::request(&pool, &alice, &bob, &follow_id)
        .await
        .unwrap();

    // carol signs an Accept for a follow of bob — not hers to accept.
    let data = fed_data(&pool).await;
    let accept: AcceptFollow = serde_json::from_value(serde_json::json!({
        "type": "Accept",
        "id": "https://other.example/accept/1",
        "actor": carol,
        "object": {
            "type": "Follow",
            "id": follow_id,
            "actor": alice,
            "object": bob,
        }
    }))
    .unwrap();
    accept.receive(&data).await.unwrap();

    assert_eq!(
        follows::following(&pool, &alice).await.unwrap(),
        vec![(bob, "pending".to_string())],
        "the follow stays pending"
    );
}

// ---- #110a: outbound remote DMs (opt-in, not private) ----------------------

/// The opt-in gate is checked before anything touches the network, so a board
/// with `allow_remote_dms = false` refuses remote mail up front — and records
/// nothing locally.
#[tokio::test]
async fn remote_dm_requires_the_opt_in() {
    use bbs_rs::config::{Federation, Limits};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let limits = Limits {
        window_secs: 0,
        ..Default::default()
    };

    // Federation on, but remote DMs off (the default).
    let fed = Federation {
        enabled: true,
        origin: "https://bbs.example.com".into(),
        allow_remote_dms: false,
        ..Default::default()
    };
    let err = bbs_rs::web::ap_object::send_remote_dm(
        &pool,
        &fed,
        &alice,
        "bob@remote.social",
        "s",
        "b",
        &limits,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("disabled"),
        "off by default: {err}"
    );

    // Federation entirely off: also refused.
    assert!(
        bbs_rs::web::ap_object::send_remote_dm(
            &pool,
            &Federation::default(),
            &alice,
            "bob@remote.social",
            "s",
            "b",
            &limits,
        )
        .await
        .is_err()
    );

    // Nothing was recorded either way.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// The local record of an outbound remote DM: a mail row from the sender to the
/// (remote) recipient, subject to the same guest/rate checks as local mail.
#[tokio::test]
async fn send_remote_records_a_local_copy() {
    use bbs_rs::config::Limits;
    use bbs_rs::services::mail;
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let bob = auth::find_user(&pool, "bob@remote.social")
        .await
        .unwrap()
        .unwrap();
    let limits = Limits {
        window_secs: 0,
        ..Default::default()
    };

    let id = mail::send_remote(&pool, &alice, &bob, "hi", "there", &limits)
        .await
        .unwrap();
    assert!(id > 0);
    let (from_id, to_id, subject): (i64, i64, String) =
        sqlx::query_as("SELECT from_id, to_id, subject FROM mail WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(from_id, alice.id);
    assert_eq!(to_id, bob.id, "addressed to the remote actor's shadow row");
    assert_eq!(subject, "hi");

    // Guests can't send, remote or otherwise.
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    assert!(matches!(
        mail::send_remote(&pool, &guest, &bob, "s", "b", &limits).await,
        Err(bbs_rs::error::AppError::GuestNotAllowed)
    ));
}

// ---- #110b: inbound remote DMs → mailbox -----------------------------------

/// A federation `Data` with remote DMs turned on.
async fn fed_data_dms(
    pool: &SqlitePool,
) -> activitypub_federation::config::Data<bbs_rs::web::ap_object::AppData> {
    use bbs_rs::config::Federation;
    use bbs_rs::services::federation::Origin;
    let fed = Federation {
        enabled: true,
        origin: "https://bbs.example.com".into(),
        allow_remote_dms: true,
        ..Default::default()
    };
    let origin = Origin::from_config(&fed).unwrap();
    bbs_rs::web::ap_object::build_config(pool.clone(), origin, &fed, &Default::default())
        .await
        .unwrap()
        .to_request_data()
}

/// Build an inbound `Create{Note}` addressed directly to `to` (no Public).
fn direct_note_from(
    actor: &str,
    note_id: &str,
    to: &str,
    summary: Option<&str>,
    content: &str,
) -> bbs_rs::web::ap_object::Create {
    let mut object = serde_json::json!({
        "type": "Note",
        "id": note_id,
        "attributedTo": actor,
        "content": content,
        "to": [to],
        "cc": [],
    });
    if let Some(s) = summary {
        object["summary"] = serde_json::json!(s);
    }
    serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": format!("{note_id}/activity"),
        "actor": actor,
        "object": object,
    }))
    .unwrap()
}

/// A direct Note addressed to a local user lands in their mailbox (opt-in on),
/// degraded to text and tagged by its remote sender.
#[tokio::test]
async fn inbound_dm_lands_in_the_mailbox() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::mail;

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice";
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;

    let data = fed_data_dms(&pool).await;
    let dm = direct_note_from(
        &bob,
        "https://remote.social/dm/1",
        alice_uri,
        Some("a private hello"),
        "<p>meet me at <a href=\"https://x.example/\">the spot</a></p>",
    );
    dm.receive(&data).await.unwrap();

    let inbox = mail::inbox(&pool, alice.id).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].from_name, "bob@remote.social",
        "sender is the remote handle (UI flags it)"
    );
    assert_eq!(
        inbox[0].subject, "a private hello",
        "summary becomes the subject"
    );
    assert_eq!(
        inbox[0].body, "meet me at the spot (https://x.example/)",
        "HTML degraded"
    );
    assert_eq!(mail::unread_count(&pool, alice.id).await.unwrap(), 1);
}

/// With the opt-in off (the default), inbound DMs are dropped, not stored.
#[tokio::test]
async fn inbound_dm_is_dropped_when_the_opt_in_is_off() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::mail;

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;

    // fed_data() (from Slice C tests) uses the default: allow_remote_dms = false.
    let data = fed_data(&pool).await;
    let dm = direct_note_from(
        &bob,
        "https://remote.social/dm/2",
        "https://bbs.example.com/u/alice",
        None,
        "<p>psst</p>",
    );
    dm.receive(&data).await.unwrap();

    assert!(mail::inbox(&pool, alice.id).await.unwrap().is_empty());
}

/// A public status (addressed to Public) is never treated as a DM — it goes to
/// the timeline, not the mailbox, even with remote DMs on.
#[tokio::test]
async fn a_public_note_is_not_a_dm() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, timeline};
    use bbs_rs::services::mail;
    use bbs_rs::web::ap_object::Create;

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    follows::accept(&pool, &alice_uri, &bob, "f1")
        .await
        .unwrap();

    let data = fed_data_dms(&pool).await;
    // Public status that also cc's alice — the Public URI makes it a status.
    let create: Create = serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/s/9/activity",
        "actor": bob,
        "object": {
            "type": "Note",
            "id": "https://remote.social/s/9",
            "attributedTo": bob,
            "content": "<p>hi all</p>",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "cc": [alice_uri],
        }
    }))
    .unwrap();
    create.receive(&data).await.unwrap();

    assert!(
        mail::inbox(&pool, alice.id).await.unwrap().is_empty(),
        "not a DM"
    );
    assert_eq!(timeline::count(&pool).await.unwrap(), 1, "it's a status");
}

/// A redelivered DM (same Note id) stores once.
#[tokio::test]
async fn a_redelivered_dm_is_stored_once() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::mail;

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data_dms(&pool).await;

    let make = || {
        direct_note_from(
            &bob,
            "https://remote.social/dm/7",
            "https://bbs.example.com/u/alice",
            None,
            "<p>only once</p>",
        )
    };
    make().receive(&data).await.unwrap();
    make().receive(&data).await.unwrap();

    assert_eq!(mail::inbox(&pool, alice.id).await.unwrap().len(), 1);
}

// ---- #111a: boards as Group actors (FEP-1b12) ------------------------------

/// A board's Group identity is minted once and reused; colliding slugs are
/// disambiguated with an id suffix.
#[tokio::test]
async fn group_keys_are_minted_once_and_slugs_stay_unique() {
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::{Origin, ensure_group_keys};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();

    let k1 = ensure_group_keys(&pool, &origin, general.id).await.unwrap();
    assert_eq!(k1.slug, "general");
    assert_eq!(k1.actor_uri, "https://bbs.example.com/c/general");
    assert!(k1.private_key.contains("BEGIN PRIVATE KEY"));
    let k2 = ensure_group_keys(&pool, &origin, general.id).await.unwrap();
    assert_eq!(k1.private_key, k2.private_key, "keypair never regenerated");

    // Two boards whose names slugify identically get distinct slugs.
    for name in ["Rust", "Rust!"] {
        sqlx::query(
            "INSERT INTO boards (name, description, min_read_role, min_write_role, locked) \
             VALUES (?, '', 'guest', 'user', 0)",
        )
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    let boards_ = boards::list_boards(&pool).await.unwrap();
    let rust: Vec<_> = boards_
        .iter()
        .filter(|b| b.name.starts_with("Rust"))
        .collect();
    let s1 = ensure_group_keys(&pool, &origin, rust[0].id)
        .await
        .unwrap()
        .slug;
    let s2 = ensure_group_keys(&pool, &origin, rust[1].id)
        .await
        .unwrap()
        .slug;
    assert_ne!(s1, s2, "colliding slugs must be disambiguated");
    assert!(s1 == "rust" || s2 == "rust");
}

/// A board is served as a `Group` actor at `/c/{slug}`.
#[tokio::test]
async fn a_board_is_served_as_a_group_actor() {
    use bbs_rs::services::federation::{Origin, ensure_all_group_keys};
    let pool = setup().await;
    ensure_all_group_keys(&pool, &Origin::new("https://bbs.example.com"))
        .await
        .unwrap();
    let base = serve_ap(pool.clone(), enabled_fed()).await;

    let res = reqwest::get(format!("{base}/c/general")).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "application/activity+json");
    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["type"], "Group");
    assert_eq!(b["id"], "https://bbs.example.com/c/general");
    assert_eq!(b["preferredUsername"], "general");
    assert_eq!(b["name"], "General");
    assert_eq!(b["inbox"], "https://bbs.example.com/c/general/inbox");
    assert_eq!(b["outbox"], "https://bbs.example.com/c/general/outbox");
    assert_eq!(
        b["followers"],
        "https://bbs.example.com/c/general/followers"
    );
    assert_eq!(
        b["manuallyApprovesFollowers"], false,
        "a board auto-accepts subscribers"
    );
    assert!(
        b["publicKey"]["publicKeyPem"]
            .as_str()
            .unwrap()
            .contains("BEGIN PUBLIC KEY")
    );

    assert_eq!(
        reqwest::get(format!("{base}/c/nope"))
            .await
            .unwrap()
            .status(),
        404
    );
}

/// WebFinger resolves a board handle (`acct:general@host`) to its Group actor.
#[tokio::test]
async fn webfinger_resolves_a_board_group() {
    use bbs_rs::services::federation::{Origin, ensure_all_group_keys};
    let pool = setup().await;
    ensure_all_group_keys(&pool, &Origin::new("https://bbs.example.com"))
        .await
        .unwrap();
    let base = serve_ap(pool.clone(), enabled_fed()).await;

    let res = reqwest::get(format!(
        "{base}/.well-known/webfinger?resource=acct:general@bbs.example.com"
    ))
    .await
    .unwrap();
    assert_eq!(res.status(), 200);
    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["subject"], "acct:general@bbs.example.com");
    let hrefs: Vec<&str> = b["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["href"].as_str())
        .collect();
    assert!(
        hrefs.contains(&"https://bbs.example.com/c/general"),
        "self link points at the Group: {hrefs:?}"
    );
}

/// The Group outbox lists the board's root posts as `Announce{Create{Page}}`.
#[tokio::test]
async fn a_group_outbox_announces_board_posts() {
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::{Origin, ensure_all_group_keys};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Hello board",
        "first post & only",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    ensure_all_group_keys(&pool, &Origin::new("https://bbs.example.com"))
        .await
        .unwrap();
    let base = serve_ap(pool.clone(), enabled_fed()).await;

    let res = reqwest::get(format!("{base}/c/general/outbox"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let b: serde_json::Value = res.json().await.unwrap();
    assert_eq!(b["type"], "OrderedCollection");
    assert_eq!(b["totalItems"], 1);
    let ann = &b["orderedItems"][0];
    assert_eq!(ann["type"], "Announce");
    assert_eq!(ann["actor"], "https://bbs.example.com/c/general");
    let create = &ann["object"];
    assert_eq!(create["type"], "Create");
    assert_eq!(create["actor"], "https://bbs.example.com/u/alice");
    let page = &create["object"];
    assert_eq!(page["type"], "Page");
    assert_eq!(page["name"], "Hello board");
    assert_eq!(page["content"], "<p>first post &amp; only</p>");
    assert_eq!(page["audience"], "https://bbs.example.com/c/general");
}

// ---- #111b: Group follow + Announce fan-out --------------------------------

/// The General board, minted as a Group. Returns its GroupKeys.
async fn mint_general_group(pool: &SqlitePool) -> bbs_rs::services::federation::GroupKeys {
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::{Origin, ensure_group_keys};
    let general = boards::list_boards(pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    ensure_group_keys(pool, &Origin::new("https://bbs.example.com"), general.id)
        .await
        .unwrap()
}

/// A board Group is a first-class signing actor: its keys resolve out of the
/// `boards` table so the drain can sign its Announce/Accept.
#[tokio::test]
async fn a_board_group_is_a_signing_actor() {
    use activitypub_federation::traits::Object;
    use bbs_rs::web::ap_object::FedActor;
    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let data = fed_data(&pool).await;

    let actor = FedActor::read_from_id(url::Url::parse(&keys.actor_uri).unwrap(), &data)
        .await
        .unwrap()
        .expect("group resolves as an actor");
    assert!(actor.local, "our own board Group is local");
    assert_eq!(actor.username, "general");
    assert!(actor.private_key.is_some(), "a Group can sign");
    assert_eq!(
        actor.inbox.as_str(),
        "https://bbs.example.com/c/general/inbox"
    );
}

/// An inbound `Follow` of a board Group is stored and answered with an `Accept`
/// signed by the Group.
#[tokio::test]
async fn inbound_follow_of_a_board_is_accepted() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, queue};
    use bbs_rs::web::ap_object::Follow;
    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let peer = insert_follower(&pool, "peer@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;

    let follow: Follow = serde_json::from_value(serde_json::json!({
        "type": "Follow",
        "id": "https://remote.social/f/1",
        "actor": peer,
        "object": keys.actor_uri,
    }))
    .unwrap();
    follow.receive(&data).await.unwrap();

    assert_eq!(follows::count(&pool, &keys.actor_uri).await.unwrap(), 1);
    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(due.len(), 1, "an Accept is queued to the subscriber");
    assert_eq!(due[0].actor_uri, keys.actor_uri, "signed by the Group");
    assert_eq!(due[0].inbox_url, "https://remote.social/users/peer/inbox");
    let v: serde_json::Value = serde_json::from_str(&due[0].activity).unwrap();
    assert_eq!(v["type"], "Accept");
    assert_eq!(v["actor"], keys.actor_uri);
    assert_eq!(v["object"]["type"], "Follow");
}

/// Posting a board root post announces it (as the Group) to every subscriber
/// inbox; a reply does not syndicate yet.
#[tokio::test]
async fn a_board_post_announces_to_subscribers() {
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::{Origin, follows, outbound, queue};
    let pool = setup().await;
    let origin = Origin::new("https://bbs.example.com");
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let keys = mint_general_group(&pool).await;
    let peer = insert_follower(&pool, "peer@remote.social", "remote.social", None).await;
    follows::accept(&pool, &peer, &keys.actor_uri, "f1")
        .await
        .unwrap();

    let mid = boards::post_message(
        &pool,
        general.id,
        &alice,
        "Hello board",
        "world & <friends>",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    let n = outbound::deliver_board_post(&pool, &origin, mid)
        .await
        .unwrap();
    assert_eq!(n, 1);

    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].actor_uri, keys.actor_uri, "the Group signs it");
    assert_eq!(due[0].inbox_url, "https://remote.social/users/peer/inbox");
    let v: serde_json::Value = serde_json::from_str(&due[0].activity).unwrap();
    assert_eq!(v["type"], "Announce");
    assert_eq!(v["actor"], keys.actor_uri);
    assert_eq!(v["object"]["type"], "Create");
    let page = &v["object"]["object"];
    assert_eq!(page["type"], "Page");
    assert_eq!(page["name"], "Hello board");
    assert_eq!(page["content"], "<p>world &amp; &lt;friends&gt;</p>");
    assert_eq!(page["attributedTo"], "https://bbs.example.com/u/alice");

    // A reply is a Note, not a Page — it doesn't syndicate in this slice.
    let rid = boards::post_message(
        &pool,
        general.id,
        &alice,
        "re",
        "a reply",
        Some(mid),
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        outbound::deliver_board_post(&pool, &origin, rid)
            .await
            .unwrap(),
        0,
        "replies don't syndicate yet"
    );
}

// ---- #111c: mirror a followed remote board ---------------------------------

/// Build an inbound `Announce{Create{Page}}` from a remote board Group.
fn board_announce_from(
    group_uri: &str,
    page_id: &str,
    author_uri: &str,
    name: &str,
    content: &str,
) -> bbs_rs::web::ap_object::Announce {
    serde_json::from_value(serde_json::json!({
        "type": "Announce",
        "id": format!("{group_uri}/announce/1"),
        "actor": group_uri,
        "object": {
            "type": "Create",
            "object": {
                "type": "Page",
                "id": page_id,
                "attributedTo": author_uri,
                "name": name,
                "content": content,
                "published": "2026-07-01T12:00:00Z",
            }
        }
    }))
    .unwrap()
}

/// A post announced by a followed remote board is degraded and mirrored.
#[tokio::test]
async fn a_followed_board_post_is_mirrored() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    // alice follows a remote board Group.
    let group = insert_follower(&pool, "rustaceans@remote.social", "remote.social", None).await;
    follows::accept(&pool, &alice_uri, &group, "https://bbs.example.com/f/1")
        .await
        .unwrap();

    let data = fed_data(&pool).await;
    let announce = board_announce_from(
        &group,
        "https://remote.social/p/1",
        "https://remote.social/users/bob",
        "Remote hello",
        "<p>from &lt;afar&gt; &amp; back</p>",
    );
    announce.receive(&data).await.unwrap();

    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);
    let posts = mirror::recent(&pool, &group, 10).await.unwrap();
    let p = &posts[0];
    assert_eq!(p.ap_id, "https://remote.social/p/1");
    assert_eq!(p.group_handle, "rustaceans@remote.social");
    assert_eq!(
        p.author_handle, "bob@remote.social",
        "author derived from its URI"
    );
    assert_eq!(p.subject, "Remote hello");
    assert_eq!(p.content, "from <afar> & back", "HTML degraded");
    assert_eq!(p.published, 1_782_907_200);
}

/// An Announce from a board nobody here follows is dropped.
#[tokio::test]
async fn an_unfollowed_board_announce_is_dropped() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let group = insert_follower(&pool, "rustaceans@remote.social", "remote.social", None).await;
    // No follow row.
    let data = fed_data(&pool).await;
    let announce = board_announce_from(
        &group,
        "https://remote.social/p/2",
        "https://remote.social/users/bob",
        "spam",
        "<p>unsolicited</p>",
    );
    announce.receive(&data).await.unwrap();
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 0);
}

/// A redelivered board post mirrors once (idempotent on the Page's id).
#[tokio::test]
async fn a_redelivered_board_post_mirrors_once() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    let group = insert_follower(&pool, "rustaceans@remote.social", "remote.social", None).await;
    follows::accept(&pool, &alice_uri, &group, "https://bbs.example.com/f/1")
        .await
        .unwrap();
    let data = fed_data(&pool).await;

    let make = || {
        board_announce_from(
            &group,
            "https://remote.social/p/9",
            "https://remote.social/users/bob",
            "once",
            "<p>only once</p>",
        )
    };
    make().receive(&data).await.unwrap();
    make().receive(&data).await.unwrap();
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);
}

// ---- #112a: inbound board posts --------------------------------------------

/// Build an inbound `Create` for a board post. `audience` is what routes it.
fn board_post_from(
    actor: &str,
    page_id: &str,
    audience: Option<&str>,
    to: &[&str],
    name: &str,
    content: &str,
    in_reply_to: Option<&str>,
) -> bbs_rs::web::ap_object::Create {
    let mut object = serde_json::json!({
        "type": "Page",
        "id": page_id,
        "attributedTo": actor,
        "name": name,
        "content": content,
        "to": to,
    });
    if let Some(a) = audience {
        object["audience"] = serde_json::json!(a);
    }
    if let Some(r) = in_reply_to {
        object["inReplyTo"] = serde_json::json!(r);
    }
    serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": format!("{page_id}/activity"),
        "actor": actor,
        "object": object,
    }))
    .unwrap()
}

/// A remote post addressed to one of our board Groups lands on that board, with
/// its HTML degraded to text and the remote author attached.
#[tokio::test]
async fn a_remote_post_lands_on_the_board() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;

    let create = board_post_from(
        &bob,
        "https://remote.social/p/1",
        Some(&keys.actor_uri),
        &["https://www.w3.org/ns/activitystreams#Public"],
        "Hello from afar",
        "<p>a remote &amp; <b>bold</b> post</p>",
        None,
    );
    create.receive(&data).await.unwrap();

    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let msgs = boards::list_messages(&pool, general.id).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].subject, "Hello from afar");
    assert_eq!(
        msgs[0].body, "a remote & bold post",
        "HTML degraded on the way in — we never store remote markup"
    );
    assert_eq!(msgs[0].author_name, "bob@remote.social");
    assert!(msgs[0].parent_id.is_none());
}

/// The Lemmy weakness this project set out to beat: a post that arrives at a
/// *person's* inbox still reaches the board, because we route on `audience`,
/// not on which inbox it was delivered to.
#[tokio::test]
async fn a_post_delivered_to_a_person_still_reaches_the_board_by_audience() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;

    // Addressed to alice (a Person), but audience is the board.
    let create = board_post_from(
        &bob,
        "https://remote.social/p/2",
        Some(&keys.actor_uri),
        &["https://bbs.example.com/u/alice"],
        "Reply via a person inbox",
        "<p>still lands on the board</p>",
        None,
    );
    create.receive(&data).await.unwrap();

    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    assert_eq!(
        boards::list_messages(&pool, general.id)
            .await
            .unwrap()
            .len(),
        1
    );
    // ...and it did NOT become a DM to alice.
    assert!(
        bbs_rs::services::mail::inbox(&pool, alice.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A remote reply threads under the post it answers, matched by `inReplyTo`.
#[tokio::test]
async fn a_remote_reply_threads_under_its_parent() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;

    board_post_from(
        &bob,
        "https://remote.social/p/10",
        Some(&keys.actor_uri),
        &[],
        "Root",
        "<p>root</p>",
        None,
    )
    .receive(&data)
    .await
    .unwrap();
    board_post_from(
        &bob,
        "https://remote.social/p/11",
        Some(&keys.actor_uri),
        &[],
        "Re: Root",
        "<p>a reply</p>",
        Some("https://remote.social/p/10"),
    )
    .receive(&data)
    .await
    .unwrap();

    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let msgs = boards::list_messages(&pool, general.id).await.unwrap();
    assert_eq!(msgs.len(), 2);
    let root = msgs.iter().find(|m| m.subject == "Root").unwrap();
    let reply = msgs.iter().find(|m| m.subject == "Re: Root").unwrap();
    assert_eq!(
        reply.parent_id,
        Some(root.id),
        "the reply threads under the post it answers"
    );
}

/// A redelivered board post stores once, and a post whose author isn't the
/// signer is ignored.
#[tokio::test]
async fn board_posts_are_deduped_and_author_is_verified() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;
    let data = fed_data(&pool).await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();

    let make = || {
        board_post_from(
            &bob,
            "https://remote.social/p/20",
            Some(&keys.actor_uri),
            &[],
            "once",
            "<p>only once</p>",
            None,
        )
    };
    make().receive(&data).await.unwrap();
    make().receive(&data).await.unwrap();
    assert_eq!(
        boards::list_messages(&pool, general.id)
            .await
            .unwrap()
            .len(),
        1,
        "redelivery stores once"
    );

    // bob signs, but the post claims to be carol's.
    let forged: bbs_rs::web::ap_object::Create = serde_json::from_value(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/p/21/activity",
        "actor": bob,
        "object": {
            "type": "Page",
            "id": "https://remote.social/p/21",
            "attributedTo": carol,
            "name": "forged",
            "content": "<p>not mine</p>",
            "audience": keys.actor_uri,
        }
    }))
    .unwrap();
    forged.receive(&data).await.unwrap();
    assert_eq!(
        boards::list_messages(&pool, general.id)
            .await
            .unwrap()
            .len(),
        1,
        "a post attributed to someone other than the signer is refused"
    );
}

/// A remote author gets the same per-window post budget as a local one — a
/// peer's own rate limiting isn't something we take on faith.
#[tokio::test]
async fn inbound_board_posts_are_rate_limited() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    // fed_data uses the default limits: max_posts = 5 per 60s window.
    let data = fed_data(&pool).await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();

    for n in 0..8 {
        board_post_from(
            &bob,
            &format!("https://remote.social/p/flood{n}"),
            Some(&keys.actor_uri),
            &[],
            &format!("flood {n}"),
            "<p>spam</p>",
            None,
        )
        .receive(&data)
        .await
        .unwrap();
    }

    let stored = boards::list_messages(&pool, general.id)
        .await
        .unwrap()
        .len();
    assert_eq!(
        stored,
        bbs_rs::config::Limits::default().max_posts as usize,
        "a flooding remote author is capped at the configured per-window budget"
    );
}

// ---- #112b: honoring remote Delete / Update --------------------------------

/// Seed a board post authored remotely, and return (board_id, ap_id).
async fn seed_remote_board_post(pool: &SqlitePool, author_uri: &str, ap_id: &str) -> i64 {
    use activitypub_federation::traits::Activity;
    let keys = mint_general_group(pool).await;
    let data = fed_data(pool).await;
    board_post_from(
        author_uri,
        ap_id,
        Some(&keys.actor_uri),
        &[],
        "Original subject",
        "<p>original body</p>",
        None,
    )
    .receive(&data)
    .await
    .unwrap();
    bbs_rs::services::boards::list_boards(pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap()
        .id
}

/// An author can withdraw their own board post; the row (and its search index
/// entry) goes with it.
#[tokio::test]
async fn a_remote_author_can_delete_their_board_post() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::web::ap_object::Delete;

    let pool = setup().await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let board = seed_remote_board_post(&pool, &bob, "https://remote.social/p/1").await;
    assert_eq!(boards::list_messages(&pool, board).await.unwrap().len(), 1);

    let data = fed_data(&pool).await;
    let del: Delete = serde_json::from_value(serde_json::json!({
        "type": "Delete",
        "id": "https://remote.social/d/1",
        "actor": bob,
        "object": "https://remote.social/p/1",
    }))
    .unwrap();
    del.receive(&data).await.unwrap();

    assert!(
        boards::list_messages(&pool, board)
            .await
            .unwrap()
            .is_empty()
    );
    // The FTS index dropped it too — a deleted remote post can't resurface in search.
    let hits = bbs_rs::services::search::search_messages(&pool, "user", "original", 10)
        .await
        .unwrap();
    assert!(hits.is_empty(), "deleted post must leave the search index");
}

/// One actor cannot delete another's content — authorization is in the SQL, so
/// a foreign Delete is simply a no-op.
#[tokio::test]
async fn a_delete_from_the_wrong_actor_is_refused() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::web::ap_object::Delete;

    let pool = setup().await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let carol = insert_follower(&pool, "carol@other.example", "other.example", None).await;
    let board = seed_remote_board_post(&pool, &bob, "https://remote.social/p/2").await;

    let data = fed_data(&pool).await;
    let del: Delete = serde_json::from_value(serde_json::json!({
        "type": "Delete",
        "id": "https://other.example/d/1",
        "actor": carol,
        "object": "https://remote.social/p/2",
    }))
    .unwrap();
    del.receive(&data).await.unwrap();

    assert_eq!(
        boards::list_messages(&pool, board).await.unwrap().len(),
        1,
        "carol cannot delete bob's post"
    );
}

/// A remote edit is applied, and its new content is degraded like the original.
#[tokio::test]
async fn a_remote_author_can_update_their_board_post() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::web::ap_object::Update;

    let pool = setup().await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let board = seed_remote_board_post(&pool, &bob, "https://remote.social/p/3").await;

    let data = fed_data(&pool).await;
    let upd: Update = serde_json::from_value(serde_json::json!({
        "type": "Update",
        "id": "https://remote.social/u/1",
        "actor": bob,
        "object": {
            "type": "Page",
            "id": "https://remote.social/p/3",
            "name": "Edited subject",
            "content": "<p>edited &amp; <b>degraded</b></p>",
        }
    }))
    .unwrap();
    upd.receive(&data).await.unwrap();

    let msgs = boards::list_messages(&pool, board).await.unwrap();
    assert_eq!(msgs[0].subject, "Edited subject");
    assert_eq!(
        msgs[0].body, "edited & degraded",
        "the edit is degraded like any other remote content"
    );
}

/// Delete also reaches cached statuses and mirrored board posts — every
/// federated store, authorized by owner.
#[tokio::test]
async fn delete_reaches_statuses_and_mirrored_posts() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, mirror, timeline};
    use bbs_rs::web::ap_object::Delete;

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let alice_uri = "https://bbs.example.com/u/alice".to_string();
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let group = insert_follower(&pool, "rustaceans@remote.social", "remote.social", None).await;
    follows::accept(&pool, &alice_uri, &bob, "f1")
        .await
        .unwrap();
    follows::accept(&pool, &alice_uri, &group, "f2")
        .await
        .unwrap();
    let data = fed_data(&pool).await;

    // A cached status from bob...
    serde_json::from_value::<bbs_rs::web::ap_object::Create>(serde_json::json!({
        "type": "Create",
        "id": "https://remote.social/a/1",
        "actor": bob,
        "object": {
            "type": "Note",
            "id": "https://remote.social/s/1",
            "attributedTo": bob,
            "content": "<p>a status</p>",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        }
    }))
    .unwrap()
    .receive(&data)
    .await
    .unwrap();
    // ...and a mirrored post announced by the board.
    board_announce_from(
        &group,
        "https://remote.social/bp/1",
        &bob,
        "mirrored",
        "<p>mirrored body</p>",
    )
    .receive(&data)
    .await
    .unwrap();
    assert_eq!(timeline::count(&pool).await.unwrap(), 1);
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);

    // bob deletes his status; the board withdraws the mirrored post.
    for (actor, object) in [
        (&bob, "https://remote.social/s/1"),
        (&group, "https://remote.social/bp/1"),
    ] {
        let del: Delete = serde_json::from_value(serde_json::json!({
            "type": "Delete",
            "id": format!("{object}/delete"),
            "actor": actor,
            "object": object,
        }))
        .unwrap();
        del.receive(&data).await.unwrap();
    }

    assert_eq!(timeline::count(&pool).await.unwrap(), 0, "status withdrawn");
    assert_eq!(
        mirror::count(&pool, &group).await.unwrap(),
        0,
        "mirrored post withdrawn by the announcing board"
    );
}

// ---- #112c: moderation surface ---------------------------------------------

/// An inbound `Flag` is filed for an operator — recorded, never acted on
/// automatically — and a redelivered report lands once.
#[tokio::test]
async fn an_inbound_report_is_filed_for_operators() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::moderation;
    use bbs_rs::web::ap_object::Flag;

    let pool = setup().await;
    let reporter = insert_follower(&pool, "mod@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;

    let make = || {
        serde_json::from_value::<Flag>(serde_json::json!({
            "type": "Flag",
            "id": "https://remote.social/flags/1",
            "actor": reporter,
            "object": ["https://bbs.example.com/p/1", "https://bbs.example.com/u/alice"],
            "content": "<p>spam &amp; abuse</p>",
        }))
        .unwrap()
    };
    make().receive(&data).await.unwrap();
    make().receive(&data).await.unwrap();

    assert_eq!(moderation::open_report_count(&pool).await.unwrap(), 1);
    let reports = moderation::reports(&pool, false, 10).await.unwrap();
    let r = &reports[0];
    assert_eq!(r.reporter_handle, "mod@remote.social");
    assert_eq!(
        r.content, "spam & abuse",
        "the comment is degraded like any remote text"
    );
    assert_eq!(r.objects.lines().count(), 2, "both reported objects kept");
    assert!(r.resolved_at.is_none());

    assert!(moderation::resolve_report(&pool, r.id).await.unwrap());
    assert_eq!(moderation::open_report_count(&pool).await.unwrap(), 0);
    assert!(
        !moderation::resolve_report(&pool, r.id).await.unwrap(),
        "resolving twice is a no-op"
    );
}

/// `silence` is the middle setting: the domain may still federate, but its
/// content stops entering shared surfaces. `suspend` is refused at the door.
#[tokio::test]
async fn silence_blocks_content_while_suspend_blocks_the_domain() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::policy;

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;
    let board = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap()
        .id;

    // Silenced: still federates (not refused at the door)...
    policy::set(&pool, "remote.social", "block", "noisy", "silence")
        .await
        .unwrap();
    assert!(
        policy::domain_allowed(&pool, "bbs.example.com", "remote.social", false)
            .await
            .unwrap(),
        "a silenced domain may still federate"
    );
    assert!(
        policy::domain_silenced(&pool, "remote.social")
            .await
            .unwrap()
    );

    // ...but its board post is dropped.
    board_post_from(
        &bob,
        "https://remote.social/p/silenced",
        Some(&keys.actor_uri),
        &[],
        "should not land",
        "<p>nope</p>",
        None,
    )
    .receive(&data)
    .await
    .unwrap();
    assert!(
        boards::list_messages(&pool, board)
            .await
            .unwrap()
            .is_empty(),
        "a silenced domain's content stays out of the board"
    );

    // Suspended: refused outright, in either posture.
    policy::set(&pool, "remote.social", "block", "abuse", "suspend")
        .await
        .unwrap();
    assert!(
        !policy::domain_allowed(&pool, "bbs.example.com", "remote.social", false)
            .await
            .unwrap(),
        "a suspended domain is refused at the door"
    );
}

/// Blocking is not retroactive — purging is the explicit, separate act.
#[tokio::test]
async fn purging_a_domain_removes_what_already_arrived() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::{moderation, policy};

    let pool = setup().await;
    let keys = mint_general_group(&pool).await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    let data = fed_data(&pool).await;
    let board = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap()
        .id;

    board_post_from(
        &bob,
        "https://remote.social/p/kept",
        Some(&keys.actor_uri),
        &[],
        "already here",
        "<p>arrived before the block</p>",
        None,
    )
    .receive(&data)
    .await
    .unwrap();
    assert_eq!(boards::list_messages(&pool, board).await.unwrap().len(), 1);

    // Blocking stops what comes next; the existing post stays.
    policy::set(&pool, "remote.social", "block", "abuse", "suspend")
        .await
        .unwrap();
    assert_eq!(
        boards::list_messages(&pool, board).await.unwrap().len(),
        1,
        "defederation is not retroactive"
    );

    // Purging is the deliberate cleanup.
    let purged = moderation::purge_domain(&pool, "remote.social")
        .await
        .unwrap();
    assert_eq!(purged.board_posts, 1);
    assert!(
        boards::list_messages(&pool, board)
            .await
            .unwrap()
            .is_empty()
    );
}

// ---- #133: Announce-wrapped lifecycle ---------------------------------------

/// Subscribe to a remote board and mirror one post from it. Returns the Group's
/// actor URI.
async fn subscribe_and_mirror(
    pool: &SqlitePool,
    group_handle: &str,
    domain: &str,
    author_uri: &str,
    page_id: &str,
) -> String {
    use bbs_rs::services::federation::{follows, mirror};

    mint_local(pool, "alice").await;
    let group = insert_follower(pool, group_handle, domain, None).await;
    follows::accept(
        pool,
        "https://bbs.example.com/u/alice",
        &group,
        &format!("https://bbs.example.com/f/{group_handle}"),
    )
    .await
    .unwrap();
    mirror::insert(
        pool,
        page_id,
        &group,
        group_handle,
        "bob@remote.social",
        author_uri,
        "Original subject",
        "original body",
        None,
        1_782_907_200,
    )
    .await
    .unwrap();
    group
}

/// Wrap an activity in an `Announce` from `group_uri`.
fn announce_wrapping(
    group_uri: &str,
    inner: serde_json::Value,
) -> bbs_rs::web::ap_object::Announce {
    serde_json::from_value(serde_json::json!({
        "type": "Announce",
        "id": format!("{group_uri}/announce/lifecycle"),
        "actor": group_uri,
        "object": inner,
    }))
    .unwrap()
}

fn delete_of(actor: &str, object: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Delete",
        "id": format!("{actor}/d/1"),
        "actor": actor,
        "object": object,
    })
}

/// The case #133 exists for: a board relays one of its members' deletions, and
/// the post leaves our mirror instead of lingering forever.
#[tokio::test]
async fn a_board_can_relay_its_members_delete() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let bob = "https://remote.social/users/bob";
    let group = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        bob,
        "https://remote.social/p/1",
    )
    .await;
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);

    let data = fed_data(&pool).await;
    announce_wrapping(&group, delete_of(bob, "https://remote.social/p/1"))
        .receive(&data)
        .await
        .unwrap();

    assert_eq!(
        mirror::count(&pool, &group).await.unwrap(),
        0,
        "the board's relayed Delete withdrew its member's post"
    );
}

/// A board may withdraw a post it authored itself, not just its members'.
#[tokio::test]
async fn a_board_can_relay_a_delete_of_its_own_post() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let group = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        "https://remote.social/c/rustaceans",
        "https://remote.social/p/2",
    )
    .await;

    let data = fed_data(&pool).await;
    announce_wrapping(&group, delete_of(&group, "https://remote.social/p/2"))
        .receive(&data)
        .await
        .unwrap();
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 0);
}

/// The authorization #133 is really about: the Group signs the relay, but that
/// does not let it withdraw content it does not host.
#[tokio::test]
async fn a_board_cannot_withdraw_another_boards_post() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    let bob = "https://remote.social/users/bob";
    let host = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        bob,
        "https://remote.social/p/3",
    )
    .await;
    // A second board we also follow, which hosts nothing of bob's.
    let meddler = insert_follower(&pool, "gossip@other.example", "other.example", None).await;
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &meddler,
        "https://bbs.example.com/f/gossip",
    )
    .await
    .unwrap();

    let data = fed_data(&pool).await;
    announce_wrapping(&meddler, delete_of(bob, "https://remote.social/p/3"))
        .receive(&data)
        .await
        .unwrap();

    assert_eq!(
        mirror::count(&pool, &host).await.unwrap(),
        1,
        "a board can only act on posts it announced"
    );
}

/// A board cannot relay a stranger's withdrawal of one of its members' posts —
/// the inner actor is checked, not just the signing Group.
#[tokio::test]
async fn a_relayed_delete_from_a_third_party_is_refused() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let group = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        "https://remote.social/users/bob",
        "https://remote.social/p/4",
    )
    .await;

    let data = fed_data(&pool).await;
    announce_wrapping(
        &group,
        delete_of(
            "https://elsewhere.example/users/mallory",
            "https://remote.social/p/4",
        ),
    )
    .receive(&data)
    .await
    .unwrap();

    assert_eq!(
        mirror::count(&pool, &group).await.unwrap(),
        1,
        "the board vouches for its members, not for anyone who asks"
    );
}

/// A remote board has no standing over a post on one of *our* boards, however it
/// is addressed. This is the case the issue flagged as "probably not legitimate".
#[tokio::test]
async fn a_board_cannot_delete_a_post_on_our_own_board() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::boards;
    use bbs_rs::services::federation::follows;

    let pool = setup().await;
    let bob = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    // A post that lives on our board, with a permanent URI.
    let board = seed_remote_board_post(&pool, &bob, "https://remote.social/p/5").await;
    assert_eq!(boards::list_messages(&pool, board).await.unwrap().len(), 1);

    mint_local(&pool, "alice").await;
    let group = insert_follower(&pool, "gossip@other.example", "other.example", None).await;
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/gossip",
    )
    .await
    .unwrap();

    let data = fed_data(&pool).await;
    // Even naming the real author as the inner actor doesn't help: the relay
    // route only ever reaches mirrored posts.
    announce_wrapping(&group, delete_of(&bob, "https://remote.social/p/5"))
        .receive(&data)
        .await
        .unwrap();

    assert_eq!(
        boards::list_messages(&pool, board).await.unwrap().len(),
        1,
        "our boards are ours; a relay cannot moderate them"
    );
}

/// A relayed edit is applied, and its HTML is degraded exactly like a new post's.
#[tokio::test]
async fn a_board_can_relay_its_members_update() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let bob = "https://remote.social/users/bob";
    let group = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        bob,
        "https://remote.social/p/6",
    )
    .await;

    let data = fed_data(&pool).await;
    announce_wrapping(
        &group,
        serde_json::json!({
            "type": "Update",
            "id": format!("{bob}/u/1"),
            "actor": bob,
            "object": {
                "id": "https://remote.social/p/6",
                "name": "Edited subject",
                "content": "<p>edited &amp; degraded</p>",
            },
        }),
    )
    .receive(&data)
    .await
    .unwrap();

    let posts = mirror::recent(&pool, &group, 10).await.unwrap();
    assert_eq!(posts[0].subject, "Edited subject");
    assert_eq!(posts[0].content, "edited & degraded", "HTML degraded");
}

/// An `Announce` wrapping something we have no handler for is accepted and
/// ignored — the mirror is untouched and nothing errors.
#[tokio::test]
async fn an_announced_activity_we_dont_model_is_ignored() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::mirror;

    let pool = setup().await;
    let group = subscribe_and_mirror(
        &pool,
        "rustaceans@remote.social",
        "remote.social",
        "https://remote.social/users/bob",
        "https://remote.social/p/7",
    )
    .await;

    let data = fed_data(&pool).await;
    announce_wrapping(
        &group,
        serde_json::json!({
            "type": "Like",
            "id": "https://remote.social/l/1",
            "actor": "https://remote.social/users/bob",
            "object": "https://remote.social/p/7",
        }),
    )
    .receive(&data)
    .await
    .unwrap();

    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);
}

/// The outbound half: deleting a syndicated board post queues the Group's
/// `Announce{Delete}` to every subscriber, with the **author** as the inner
/// actor so the receiver can authorize it against the post's attribution.
#[tokio::test]
async fn deleting_a_syndicated_post_announces_the_withdrawal() {
    use bbs_rs::services::federation::{Origin, outbound, queue};

    let pool = setup().await;
    let fed = enabled_fed();
    let origin = Origin::from_config(&fed).unwrap();
    let alice = mint_local(&pool, "alice").await;
    let board = bbs_rs::services::boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let msg_id = bbs_rs::services::boards::post_message(
        &pool,
        board.id,
        &alice,
        "Going away",
        "temporary",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // A remote subscriber to the board Group.
    let keys = bbs_rs::services::federation::ensure_group_keys(&pool, &origin, board.id)
        .await
        .unwrap();
    let sub = insert_follower(
        &pool,
        "carol@remote.social",
        "remote.social",
        Some("https://remote.social/inbox"),
    )
    .await;
    bbs_rs::services::federation::follows::accept(
        &pool,
        &sub,
        &keys.actor_uri,
        "https://remote.social/f/1",
    )
    .await
    .unwrap();

    // Syndicate it, so the post has a permanent URI to withdraw.
    outbound::deliver_board_post(&pool, &origin, msg_id)
        .await
        .unwrap();
    let before = queue::pending(&pool).await.unwrap();

    let prepared = outbound::prepare_board_delete(&pool, &origin, msg_id)
        .await
        .unwrap()
        .expect("a syndicated post has a withdrawal to announce");
    assert!(
        bbs_rs::services::boards::delete_message(&pool, msg_id)
            .await
            .unwrap()
    );
    assert_eq!(outbound::dispatch(&pool, &prepared).await.unwrap(), 1);
    assert_eq!(queue::pending(&pool).await.unwrap(), before + 1);

    let due = queue::due(&pool, 10).await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&due.last().unwrap().activity).unwrap();
    assert_eq!(body["type"], "Announce");
    assert_eq!(body["actor"], keys.actor_uri, "the Group relays");
    assert_eq!(body["object"]["type"], "Delete");
    assert_eq!(
        body["object"]["actor"], "https://bbs.example.com/u/alice",
        "the author withdraws; the Group only carries it"
    );
    let withdrawn = body["object"]["object"].as_str().unwrap();
    assert!(
        withdrawn.starts_with("https://bbs.example.com/"),
        "withdraws our own minted URI, got {withdrawn}"
    );
}

/// A post that never federated has nothing to withdraw.
#[tokio::test]
async fn deleting_an_unsyndicated_post_announces_nothing() {
    use bbs_rs::services::federation::{Origin, outbound};

    let pool = setup().await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let alice = mint_local(&pool, "alice").await;
    let board = bbs_rs::services::boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let msg_id = bbs_rs::services::boards::post_message(
        &pool,
        board.id,
        &alice,
        "Local only",
        "never left",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        outbound::prepare_board_delete(&pool, &origin, msg_id)
            .await
            .unwrap()
            .is_none(),
        "no subscribers and no ap_id — nothing to announce"
    );
}

/// Close the loop: the `Announce{Delete}` we *emit* must deserialize as the
/// `Announce{Delete}` we *accept*, and withdraw the post on the far side.
///
/// The two shapes are built by different code (`objects::board_delete` writes
/// it, `ap_object::Announce` reads it), so nothing but a round trip proves they
/// agree — this is the bbs-rs ↔ bbs-rs case the issue is really about.
#[tokio::test]
async fn our_announced_delete_is_one_we_would_honor() {
    use activitypub_federation::traits::Activity;
    use bbs_rs::services::federation::{Origin, mirror, objects};

    let pool = setup().await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();

    // Stand in for the far side: we subscribe to a board whose actor URI is
    // exactly what our own origin would mint for "general".
    let group_uri = origin.group("general");
    let author_uri = origin.person("alice");
    let page_id = "https://bbs.example.com/m/42";
    mint_local(&pool, "carol").await;
    bbs_rs::services::federation::follows::accept(
        &pool,
        "https://bbs.example.com/u/carol",
        &group_uri,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();
    mirror::insert(
        &pool,
        page_id,
        &group_uri,
        "general@bbs.example.com",
        "alice@bbs.example.com",
        &author_uri,
        "Going away",
        "temporary",
        None,
        1_782_907_200,
    )
    .await
    .unwrap();

    // Serialize what we would send, parse it as what we would receive.
    let wire = serde_json::to_string(&objects::board_delete(
        &origin,
        "general",
        &author_uri,
        42,
        page_id,
    ))
    .unwrap();
    let inbound: bbs_rs::web::ap_object::Announce = serde_json::from_str(&wire)
        .expect("our own Announce{Delete} must parse as an inbound Announce");

    inbound.receive(&fed_data(&pool).await).await.unwrap();
    assert_eq!(
        mirror::count(&pool, &group_uri).await.unwrap(),
        0,
        "the post we announced as deleted left the mirror"
    );
}

// ---- #132: in-BBS screen for mirrored remote boards -------------------------

/// A subscribed board is listed with its post count and newest-post time.
#[tokio::test]
async fn subscribed_remote_boards_are_listed_with_stats() {
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let group = insert_follower(&pool, "rustaceans@remote.social", "remote.social", None).await;
    sqlx::query("UPDATE users SET actor_kind = 'Group' WHERE actor_uri = ?")
        .bind(&group)
        .execute(&pool)
        .await
        .unwrap();
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();
    for (i, ts) in [(1, 1_782_907_200_i64), (2, 1_782_993_600)] {
        mirror::insert(
            &pool,
            &format!("https://remote.social/p/{i}"),
            &group,
            "rustaceans@remote.social",
            "bob@remote.social",
            "https://remote.social/users/bob",
            "Subject",
            "body",
            None,
            ts,
        )
        .await
        .unwrap();
    }

    let boards = mirror::boards(&pool).await.unwrap();
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].handle, "rustaceans@remote.social");
    assert_eq!(boards[0].state, "accepted");
    assert_eq!(boards[0].posts, 2);
    assert_eq!(boards[0].latest, Some(1_782_993_600));
}

/// A followed *person* is not a board — the screen must not list them.
#[tokio::test]
async fn followed_people_are_not_listed_as_boards() {
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let person = insert_follower(&pool, "bob@remote.social", "remote.social", None).await;
    sqlx::query("UPDATE users SET actor_kind = 'Person' WHERE actor_uri = ?")
        .bind(&person)
        .execute(&pool)
        .await
        .unwrap();
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &person,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();

    assert!(
        mirror::boards(&pool).await.unwrap().is_empty(),
        "following a Person is not subscribing to a board"
    );
}

/// A board subscribed *before* migration 0019 recorded actor types has a NULL
/// kind. It must still appear, on the evidence of what it has announced —
/// otherwise upgrading would silently empty this screen.
#[tokio::test]
async fn a_board_predating_actor_kind_is_still_listed() {
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let group = insert_follower(&pool, "legacy@remote.social", "remote.social", None).await;
    // Explicitly NULL: the state an upgraded install is actually in.
    sqlx::query("UPDATE users SET actor_kind = NULL WHERE actor_uri = ?")
        .bind(&group)
        .execute(&pool)
        .await
        .unwrap();
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();
    mirror::insert(
        &pool,
        "https://remote.social/p/1",
        &group,
        "legacy@remote.social",
        "bob@remote.social",
        "https://remote.social/users/bob",
        "Subject",
        "body",
        None,
        1_782_907_200,
    )
    .await
    .unwrap();

    let boards = mirror::boards(&pool).await.unwrap();
    assert_eq!(boards.len(), 1, "proven a board by what it announced");
    assert_eq!(boards[0].posts, 1);
}

/// A pending subscription is listed with its state, so an empty board reads as
/// "not accepted yet" rather than as a bug.
#[tokio::test]
async fn a_pending_board_subscription_is_listed_as_pending() {
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let group = insert_follower(&pool, "slow@remote.social", "remote.social", None).await;
    sqlx::query("UPDATE users SET actor_kind = 'Group' WHERE actor_uri = ?")
        .bind(&group)
        .execute(&pool)
        .await
        .unwrap();
    follows::request(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();

    let boards = mirror::boards(&pool).await.unwrap();
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].state, "pending");
    assert_eq!(boards[0].posts, 0);
    assert_eq!(boards[0].latest, None);
}

/// When two local users follow the same board in different states, the board is
/// live for the instance — so it must not be a coin flip which state shows.
#[tokio::test]
async fn a_board_followed_twice_reports_the_accepted_state() {
    use bbs_rs::services::federation::{follows, mirror};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    mint_local(&pool, "bob").await;
    let group = insert_follower(&pool, "busy@remote.social", "remote.social", None).await;
    sqlx::query("UPDATE users SET actor_kind = 'Group' WHERE actor_uri = ?")
        .bind(&group)
        .execute(&pool)
        .await
        .unwrap();
    follows::request(
        &pool,
        "https://bbs.example.com/u/bob",
        &group,
        "https://bbs.example.com/f/2",
    )
    .await
    .unwrap();
    follows::accept(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();

    let boards = mirror::boards(&pool).await.unwrap();
    assert_eq!(boards.len(), 1, "one board, not one row per follower");
    assert_eq!(boards[0].state, "accepted");
}

/// Fetching a remote actor records what kind it is (migration 0019), which is
/// what lets a board be listed before it has announced anything.
#[tokio::test]
async fn fetching_a_group_actor_records_its_kind() {
    use activitypub_federation::traits::Object;

    let pool = setup().await;
    let data = fed_data(&pool).await;
    let doc: bbs_rs::web::ap_object::Person = serde_json::from_value(serde_json::json!({
        "type": "Group",
        "id": "https://remote.social/c/rustaceans",
        "preferredUsername": "rustaceans",
        "inbox": "https://remote.social/c/rustaceans/inbox",
        "publicKey": {
            "id": "https://remote.social/c/rustaceans#main-key",
            "owner": "https://remote.social/c/rustaceans",
            "publicKeyPem": "-----BEGIN PUBLIC KEY-----\nx\n-----END PUBLIC KEY-----\n",
        },
    }))
    .unwrap();
    bbs_rs::web::ap_object::FedActor::from_json(doc, &data)
        .await
        .unwrap();

    let kind: Option<String> =
        sqlx::query_scalar("SELECT actor_kind FROM users WHERE actor_uri = ?")
            .bind("https://remote.social/c/rustaceans")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kind.as_deref(), Some("Group"));
}

// ---- #131: posting into a followed remote board -----------------------------

/// Subscribe (accepted) to a remote board with a known inbox. Returns its URI.
async fn subscribe_to_board(pool: &SqlitePool, handle: &str) -> String {
    use bbs_rs::services::federation::follows;

    let group = insert_follower(pool, handle, handle.split('@').nth(1).unwrap(), None).await;
    sqlx::query("UPDATE users SET actor_kind = 'Group' WHERE actor_uri = ?")
        .bind(&group)
        .execute(pool)
        .await
        .unwrap();
    follows::accept(
        pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();
    group
}

/// A submission queues the author's signed `Create{Page}` to the board's inbox,
/// addressed so the board routes it.
#[tokio::test]
async fn posting_to_a_remote_board_queues_a_create_for_its_inbox() {
    use bbs_rs::services::federation::{Origin, queue, remote_posting};

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let group = subscribe_to_board(&pool, "rustaceans@remote.social").await;

    let ap_id = remote_posting::submit(
        &pool,
        &origin,
        &alice,
        &group,
        "Hello from bbs-rs",
        "posting across the fediverse",
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(ap_id.starts_with("https://bbs.example.com/p/"), "{ap_id}");

    let due = queue::due(&pool, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        due[0].inbox_url, "https://remote.social/users/rustaceans/inbox",
        "delivered to the board's own inbox, not a shared one — this is a \
         targeted submission, not a fan-out"
    );
    assert_eq!(
        due[0].actor_uri, "https://bbs.example.com/u/alice",
        "the author signs — we're a contributor to that board, not its hub"
    );

    let body: serde_json::Value = serde_json::from_str(&due[0].activity).unwrap();
    assert_eq!(body["type"], "Create");
    assert_eq!(body["actor"], "https://bbs.example.com/u/alice");
    assert_eq!(body["object"]["type"], "Page");
    assert_eq!(body["object"]["id"], ap_id);
    assert_eq!(body["object"]["name"], "Hello from bbs-rs");
    assert_eq!(
        body["object"]["audience"], group,
        "audience is what routes it to the board"
    );
    assert_eq!(body["object"]["to"][0], group);
    assert_eq!(
        body["object"]["cc"][0],
        "https://www.w3.org/ns/activitystreams#Public"
    );
    assert_eq!(
        body["object"]["content"], "<p>posting across the fediverse</p>",
        "body is HTML-escaped into its wrapper"
    );
}

/// Until the board announces it back, a submission shows as pending — we are not
/// the authority for that board and don't get to call it published.
#[tokio::test]
async fn a_submission_is_pending_until_the_board_announces_it_back() {
    use bbs_rs::services::federation::{Origin, mirror, remote_posting};

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let group = subscribe_to_board(&pool, "rustaceans@remote.social").await;

    let ap_id = remote_posting::submit(
        &pool,
        &origin,
        &alice,
        &group,
        "Hello",
        "body",
        &Default::default(),
    )
    .await
    .unwrap();

    let pending = remote_posting::pending(&pool, &group).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].subject, "Hello");
    assert_eq!(pending[0].author_handle, "alice");

    // The board publishes it: same ap_id arrives through the normal mirror path.
    mirror::insert(
        &pool,
        &ap_id,
        &group,
        "rustaceans@remote.social",
        "alice@bbs.example.com",
        "https://bbs.example.com/u/alice",
        "Hello",
        "body",
        None,
        1_782_907_200,
    )
    .await
    .unwrap();

    assert!(
        remote_posting::pending(&pool, &group)
            .await
            .unwrap()
            .is_empty(),
        "the published copy supersedes the pending one, with no status to sync"
    );
    assert_eq!(mirror::count(&pool, &group).await.unwrap(), 1);
}

/// Posting into a board that hasn't accepted the subscription fails loudly.
/// A fan-out can no-op silently; a direct user action must not.
#[tokio::test]
async fn posting_to_an_unaccepted_board_is_refused() {
    use bbs_rs::services::federation::{Origin, follows, queue, remote_posting};

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let group = insert_follower(&pool, "slow@remote.social", "remote.social", None).await;
    follows::request(
        &pool,
        "https://bbs.example.com/u/alice",
        &group,
        "https://bbs.example.com/f/1",
    )
    .await
    .unwrap();

    let err = remote_posting::submit(
        &pool,
        &origin,
        &alice,
        &group,
        "Hello",
        "body",
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("accepted"), "got: {err}");
    assert_eq!(queue::pending(&pool).await.unwrap(), 0, "nothing queued");
}

/// A board nobody subscribes to can't be posted to.
#[tokio::test]
async fn posting_to_an_unsubscribed_board_is_refused() {
    use bbs_rs::services::federation::{Origin, remote_posting};

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();

    let err = remote_posting::submit(
        &pool,
        &origin,
        &alice,
        "https://elsewhere.example/c/strangers",
        "Hello",
        "body",
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("not subscribed"), "got: {err}");
}

/// Guests never federate — including into someone else's board.
#[tokio::test]
async fn a_guest_cannot_post_to_a_remote_board() {
    use bbs_rs::services::federation::{Origin, remote_posting};

    let pool = setup().await;
    mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let group = subscribe_to_board(&pool, "rustaceans@remote.social").await;
    let guest = bbs_rs::services::auth::find_user(&pool, "guest")
        .await
        .unwrap()
        .expect("the guest account exists by default");

    let err = remote_posting::submit(
        &pool,
        &origin,
        &guest,
        &group,
        "Hello",
        "body",
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, bbs_rs::error::AppError::GuestNotAllowed));
}

/// Federating isn't a way around the local rate limit.
#[tokio::test]
async fn remote_board_posts_are_rate_limited_like_local_ones() {
    use bbs_rs::config::Limits;
    use bbs_rs::services::federation::{Origin, remote_posting};

    let pool = setup().await;
    let alice = mint_local(&pool, "alice").await;
    let origin = Origin::from_config(&enabled_fed()).unwrap();
    let group = subscribe_to_board(&pool, "rustaceans@remote.social").await;
    let limits = Limits {
        max_posts: 1,
        ..Default::default()
    };

    // A local post uses up the budget.
    let board = bbs_rs::services::boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    bbs_rs::services::boards::post_message(&pool, board.id, &alice, "local", "body", None, &limits)
        .await
        .unwrap();

    let err = remote_posting::submit(&pool, &origin, &alice, &group, "Hi", "body", &limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, bbs_rs::error::AppError::RateLimited),
        "got: {err}"
    );
}

/// An `Announce` we emit must carry an id on **our** origin, even when the post
/// it wraps belongs to someone else.
///
/// Regression test. The id used to be derived from the post's URI, so
/// re-announcing an inbound post (#112a) minted an activity id under the
/// *author's* domain. The author's own instance then rejected the delivery
/// outright — `Activity was sent from local instance` — meaning the one server
/// guaranteed to care about that post was the one server that could never
/// receive it. Found by the #131 two-instance run.
#[tokio::test]
async fn an_announce_of_a_remote_post_is_still_our_activity() {
    use bbs_rs::services::federation::{Origin, objects};

    let origin = Origin::from_config(&enabled_fed()).unwrap();
    // A post authored on, and hosted at, another instance.
    let foreign_page = objects::board_page(
        &origin,
        "general",
        "https://peer.example/p/7",
        "https://peer.example/u/bob",
        "From elsewhere",
        "body",
        1_782_907_200,
    );
    let announce = objects::board_announce(
        &origin,
        "general",
        "https://peer.example/u/bob",
        5,
        foreign_page,
    );

    assert!(
        announce.id.starts_with("https://bbs.example.com/"),
        "the Announce is our activity and must live on our origin, got {}",
        announce.id
    );
    assert!(
        !announce.id.starts_with("https://peer.example/"),
        "must not mint an activity id under the author's domain"
    );
    // The post itself keeps its own identity and attribution — only the
    // wrapper is ours.
    assert_eq!(announce.object.object.id, "https://peer.example/p/7");
    assert_eq!(
        announce.object.object.attributed_to,
        "https://peer.example/u/bob"
    );

    let del = objects::board_delete(
        &origin,
        "general",
        "https://peer.example/u/bob",
        5,
        "https://peer.example/p/7",
    );
    assert!(
        del.id.starts_with("https://bbs.example.com/"),
        "same rule for Announce{{Delete}}, got {}",
        del.id
    );
}
