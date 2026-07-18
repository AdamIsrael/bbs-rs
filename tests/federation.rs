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
        && let Ok(config) = bbs_rs::web::ap_object::build_config(pool, origin, &fed).await
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
    policy::set(&pool, "friend.example", "allow", "a peer")
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
    policy::set(&pool, "spam.example", "block", "spam")
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
    let config = build_config(pool.clone(), origin, &fed).await.unwrap();
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
    let config = build_config(pool.clone(), origin, &fed).await.unwrap();
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
    let config = build_config(pool.clone(), origin, &fed).await.unwrap();
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
    let config = bbs_rs::web::ap_object::build_config(pool.clone(), origin, &fed)
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
