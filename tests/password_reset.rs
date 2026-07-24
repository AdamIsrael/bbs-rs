//! Account recovery (#76): sysop-driven resets, the forced change on next
//! login, self-service rotation, and the sweeper that ends sessions predating
//! a reset.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::error::AppError;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{admin, auth};
use bbs_rs::ssh::server::enforce_password_resets;
use bbs_rs::transport::{Event, Transport};

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

async fn reg(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "original-pw", &Default::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_reset_replaces_the_password_and_flags_the_account() {
    let pool = setup().await;
    reg(&pool, "alice").await;

    assert!(
        auth::set_password(&pool, "alice", "temp-secret", true)
            .await
            .unwrap()
    );

    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();
    assert!(alice.must_change_password(), "the flag is set");
    assert!(
        auth::verify_login(&pool, "alice", "temp-secret")
            .await
            .unwrap()
            .is_some(),
        "the new password works"
    );
    assert!(
        auth::verify_login(&pool, "alice", "original-pw")
            .await
            .unwrap()
            .is_none(),
        "the old password does not"
    );
}

#[tokio::test]
async fn no_force_resets_without_flagging() {
    let pool = setup().await;
    reg(&pool, "alice").await;

    auth::set_password(&pool, "alice", "temp-secret", false)
        .await
        .unwrap();
    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();
    assert!(!alice.must_change_password());
}

#[tokio::test]
async fn resetting_a_missing_or_remote_account_is_a_no_op() {
    let pool = setup().await;
    assert!(
        !auth::set_password(&pool, "nobody", "temp-secret", true)
            .await
            .unwrap()
    );

    // A discovered ActivityPub actor lives in `users` but has no usable
    // password and must never gain one.
    sqlx::query(
        "INSERT INTO users (username, password_hash, role, created_at, validated_at, is_remote) \
         VALUES ('bob@remote.social', '!', 'user', 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !auth::set_password(&pool, "bob@remote.social", "temp-secret", true)
            .await
            .unwrap(),
        "remote actors are excluded in the WHERE clause"
    );
    let bob = auth::find_user(&pool, "bob@remote.social")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob.password_hash, "!", "the sentinel hash is untouched");
}

#[tokio::test]
async fn short_passwords_are_refused() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;

    assert!(matches!(
        auth::set_password(&pool, "alice", "short", true).await,
        Err(AppError::PasswordTooShort(_))
    ));
    assert!(matches!(
        auth::set_own_password(&pool, alice.id, "short").await,
        Err(AppError::PasswordTooShort(_))
    ));
    assert!(
        auth::verify_login(&pool, "alice", "original-pw")
            .await
            .unwrap()
            .is_some(),
        "a refused change leaves the account alone"
    );
}

#[tokio::test]
async fn self_service_change_requires_the_current_password() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;

    assert!(matches!(
        auth::change_password(&pool, alice.id, "not-it", "brand-new-pw").await,
        Err(AppError::PasswordIncorrect)
    ));
    assert!(
        auth::verify_login(&pool, "alice", "original-pw")
            .await
            .unwrap()
            .is_some(),
        "a wrong current password changes nothing"
    );

    auth::change_password(&pool, alice.id, "original-pw", "brand-new-pw")
        .await
        .unwrap();
    assert!(
        auth::verify_login(&pool, "alice", "brand-new-pw")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn setting_a_new_password_clears_the_forced_flag() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();

    // The forced path takes no current password: the session already
    // authenticated, and a pubkey login never saw the temporary one.
    auth::set_own_password(&pool, alice.id, "chosen-by-alice")
        .await
        .unwrap();

    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();
    assert!(!alice.must_change_password(), "the gate is cleared");
    assert!(
        auth::verify_login(&pool, "alice", "chosen-by-alice")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        admin::pending_password_resets(&pool)
            .await
            .unwrap()
            .is_empty(),
        "and the sweeper stops tracking it"
    );
}

/// A live session for `name` that started `since`, plus its event receiver.
async fn session(presence: &Presence, id: usize, name: &str, since: i64) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel(16);
    presence
        .join_at(id, name.to_string(), None, tx, since)
        .await;
    rx
}

#[tokio::test]
async fn the_sweep_ends_sessions_that_predate_the_reset() {
    let pool = setup().await;
    reg(&pool, "alice").await;
    let now = bbs_rs::util::now_unix();

    let presence = Presence::new();
    // Session 1 was already up when the sysop reset the password — this is the
    // intruder the reset is aimed at.
    let mut stale = session(&presence, 1, "alice", now - 600).await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();
    // Session 2 logged in afterwards with the temporary password and is sitting
    // on the change-password gate.
    let mut fresh = session(&presence, 2, "alice", now + 60).await;

    enforce_password_resets(&pool, &presence).await;

    assert!(
        matches!(stale.try_recv(), Ok(Event::Quit)),
        "the pre-reset session is ended"
    );
    assert!(
        fresh.try_recv().is_err(),
        "the session on the gate is left alone — kicking it would make the \
         account unrecoverable"
    );
}

#[tokio::test]
async fn the_sweep_leaves_unaffected_users_alone() {
    let pool = setup().await;
    reg(&pool, "alice").await;
    reg(&pool, "bob").await;
    let now = bbs_rs::util::now_unix();

    let presence = Presence::new();
    let mut bob = session(&presence, 1, "bob", now - 600).await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();

    enforce_password_resets(&pool, &presence).await;
    assert!(bob.try_recv().is_err(), "bob's password wasn't reset");

    // And with no pending resets at all the sweep is a no-op.
    auth::set_password(&pool, "alice", "temp-secret", false)
        .await
        .unwrap();
    enforce_password_resets(&pool, &presence).await;
    assert!(bob.try_recv().is_err());
}

// ---- The in-BBS gate and the self-service screen ------------------------

fn app(pool: SqlitePool, user: User) -> App {
    App::new(
        pool,
        Presence::new(),
        Arc::new(Settings::default()),
        user,
        1,
        Transport::Ssh,
    )
}

async fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        .await;
}

async fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c)).await;
    }
}

#[tokio::test]
async fn a_flagged_session_starts_on_the_gate_and_cannot_leave_it() {
    let pool = setup().await;
    reg(&pool, "alice").await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();
    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();

    let mut app = app(pool.clone(), alice);
    assert_eq!(
        app.screen,
        Screen::ChangePassword,
        "the session lands on the gate, not the menu"
    );
    assert!(app.status.contains("reset"), "and says why: {}", app.status);

    press(&mut app, KeyCode::Esc).await;
    assert_eq!(app.screen, Screen::ChangePassword, "Esc doesn't get out");
    assert!(!app.should_quit);

    // Ctrl-C still ends the session — the gate is a wall, not a trap.
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(app.should_quit);
}

#[tokio::test]
async fn setting_a_new_password_at_the_gate_opens_the_bbs() {
    let pool = setup().await;
    reg(&pool, "alice").await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();
    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();

    let mut app = app(pool.clone(), alice);
    // The forced form is two fields: new + confirm. No current password.
    assert_eq!(app.form.fields.len(), 2);
    typed(&mut app, "chosen-by-alice").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "chosen-by-alice").await;
    press(&mut app, KeyCode::Enter).await;

    assert_eq!(app.screen, Screen::MainMenu, "{}", app.status);
    assert!(!app.force_password_change);
    assert!(
        auth::verify_login(&pool, "alice", "chosen-by-alice")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_mismatch_or_short_password_holds_the_gate() {
    let pool = setup().await;
    reg(&pool, "alice").await;
    auth::set_password(&pool, "alice", "temp-secret", true)
        .await
        .unwrap();
    let alice = auth::find_user(&pool, "alice").await.unwrap().unwrap();
    let mut app = app(pool.clone(), alice);

    typed(&mut app, "chosen-by-alice").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "typo-by-alice").await;
    press(&mut app, KeyCode::Enter).await;
    assert_eq!(app.screen, Screen::ChangePassword);
    assert!(app.status.contains("do not match"), "{}", app.status);

    // The form is re-armed empty, so retry from scratch — this time too short.
    let mut app = app;
    app.form.fields.iter_mut().for_each(|f| f.value.clear());
    app.form.focus = 0;
    typed(&mut app, "abc").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "abc").await;
    press(&mut app, KeyCode::Enter).await;
    assert_eq!(app.screen, Screen::ChangePassword);
    assert!(app.status.contains("at least"), "{}", app.status);
    assert!(
        auth::verify_login(&pool, "alice", "temp-secret")
            .await
            .unwrap()
            .is_some(),
        "the temporary password still works"
    );
}

#[tokio::test]
async fn self_service_change_from_the_profile_screen() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let mut app = app(pool.clone(), alice.clone());
    assert_eq!(app.screen, Screen::MainMenu, "no reset pending");

    // Land on her own profile, then press p.
    app.current_profile = Some(
        bbs_rs::services::profiles::get_profile(&pool, alice.id)
            .await
            .unwrap(),
    );
    app.screen = Screen::Profile;
    press(&mut app, KeyCode::Char('p')).await;
    assert_eq!(app.screen, Screen::ChangePassword);
    assert!(!app.force_password_change);
    assert_eq!(
        app.form.fields.len(),
        3,
        "voluntary changes ask for the current password"
    );

    // Esc backs out of a voluntary change.
    press(&mut app, KeyCode::Esc).await;
    assert_eq!(app.screen, Screen::Profile);

    press(&mut app, KeyCode::Char('p')).await;
    typed(&mut app, "original-pw").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "second-password").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "second-password").await;
    press(&mut app, KeyCode::Enter).await;

    assert_eq!(app.screen, Screen::Profile, "{}", app.status);
    assert!(
        auth::verify_login(&pool, "alice", "second-password")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn generated_temp_passwords_are_long_and_distinct() {
    let a = auth::generate_temp_password().unwrap();
    let b = auth::generate_temp_password().unwrap();
    assert!(a.chars().count() >= auth::MIN_PASSWORD_CHARS);
    assert_ne!(a, b, "each reset gets its own password");
    assert!(
        a.chars().all(|c| c.is_ascii_alphanumeric()),
        "safe to read out loud and retype: {a}"
    );
}
