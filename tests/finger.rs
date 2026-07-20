//! The read-only finger service (#77): request → formatted response, plus the
//! per-user opt-out that hides someone from both the listing and a direct query.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::services::presence::Presence;
use bbs_rs::services::{auth, finger, profiles};

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn finger_user_shows_their_profile_card() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    profiles::update_profile(
        &pool,
        alice.id,
        "Alice Anderson",
        "Portland",
        "hi there",
        "",
    )
    .await
    .unwrap();
    let presence = Presence::new();

    let out = finger::respond(&pool, &presence, "alice").await;
    assert!(out.contains("Login: alice"), "{out}");
    assert!(out.contains("Alice Anderson"));
    assert!(out.contains("Portland"));
    assert!(out.contains("Posts: 0"));
    assert!(out.contains("Status: offline"));
    // `finger user@host` sends the bare local part, but be lenient if a client
    // sends the whole thing.
    assert!(
        finger::respond(&pool, &presence, "alice@example.com")
            .await
            .contains("Login: alice")
    );
}

#[tokio::test]
async fn finger_at_host_lists_who_is_online() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let presence = Presence::new();
    let (tx, _rx) = mpsc::channel(4);
    presence.join(1, "alice".into(), None, tx).await;

    let out = finger::respond(&pool, &presence, "").await;
    assert!(out.contains("Who's online"));
    assert!(out.contains("alice"), "{out}");
    assert!(out.contains("on for"));
}

#[tokio::test]
async fn an_opted_out_user_is_invisible_to_finger() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let presence = Presence::new();
    let (tx, _rx) = mpsc::channel(4);
    presence.join(1, "alice".into(), None, tx).await;

    profiles::set_finger_optout(&pool, alice.id, true)
        .await
        .unwrap();

    // A direct query reads as "no such user" — it can't be used to probe.
    let direct = finger::respond(&pool, &presence, "alice").await;
    assert!(direct.contains("no such user"), "{direct}");
    // And they're dropped from the online listing.
    let listing = finger::respond(&pool, &presence, "").await;
    assert!(!listing.contains("alice"), "{listing}");
    assert!(listing.contains("(nobody)"));

    // Toggling back makes them visible again.
    profiles::set_finger_optout(&pool, alice.id, false)
        .await
        .unwrap();
    assert!(
        finger::respond(&pool, &presence, "alice")
            .await
            .contains("Login: alice")
    );
}

#[tokio::test]
async fn finger_of_an_unknown_user_says_so() {
    let pool = setup().await;
    let presence = Presence::new();
    let out = finger::respond(&pool, &presence, "nobody").await;
    assert!(out.contains("no such user"), "{out}");
}

#[tokio::test]
async fn f_on_your_own_profile_toggles_finger_visibility() {
    use bbs_rs::app::App;
    use bbs_rs::app::state::Screen;
    use bbs_rs::config::Settings;
    use bbs_rs::transport::Transport;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;

    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let mut app = App::new(
        pool.clone(),
        Presence::new(),
        Arc::new(Settings::default()),
        alice.clone(),
        1,
        Transport::Ssh,
    );
    app.current_profile = Some(profiles::get_profile(&pool, alice.id).await.unwrap());
    app.screen = Screen::Profile;

    // Press f -> opt out.
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
        .await;
    assert!(
        profiles::get_profile(&pool, alice.id)
            .await
            .unwrap()
            .finger_optout
    );
    // Press f again -> back to listed.
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
        .await;
    assert!(
        !profiles::get_profile(&pool, alice.id)
            .await
            .unwrap()
            .finger_optout
    );
}
