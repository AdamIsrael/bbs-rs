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
