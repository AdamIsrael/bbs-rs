//! In-BBS federation domain moderation (#159): allow/block/silence/remove a
//! domain from the Admin · Federation screen, driven through the real App key
//! handler, and confirm each change hits the `ap_blocks` policy and the audit
//! log (#74).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::federation::policy;
use bbs_rs::services::{self, audit, auth, presence::Presence};
use bbs_rs::transport::Transport;

fn config() -> Arc<Settings> {
    Arc::new(Settings::default())
}

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    services::seed(&pool, &Default::default()).await.unwrap();
    pool
}

async fn admin(pool: &SqlitePool) -> User {
    auth::register_user(pool, "sysop", "pw", &Default::default())
        .await
        .unwrap();
    services::admin::set_role(pool, "sysop", "admin")
        .await
        .unwrap();
    auth::find_user(pool, "sysop").await.unwrap().unwrap()
}

fn app_with(pool: SqlitePool, user: User) -> App {
    App::new(pool, Presence::new(), config(), user, 1, Transport::Ssh)
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

/// Drive: open the federation screen, press the action key, type `domain`, Enter.
async fn add_entry(app: &mut App, action: char, domain: &str) {
    press(app, KeyCode::Char('f')).await; // AdminUsers -> AdminFederation
    press(app, KeyCode::Char(action)).await; // a / b / s -> composer
    typed(app, domain).await;
    press(app, KeyCode::Enter).await;
}

#[tokio::test]
async fn allow_block_and_silence_write_policy_and_audit() {
    let pool = setup().await;
    let sysop = admin(&pool).await;
    let mut app = app_with(pool.clone(), sysop);
    app.screen = Screen::AdminUsers;

    add_entry(&mut app, 'a', "friend.example").await;
    add_entry(&mut app, 'b', "spam.example").await;
    add_entry(&mut app, 's', "noisy.example").await;

    assert!(matches!(app.screen, Screen::AdminFederation));

    // Policy rows.
    let allows = policy::list(&pool, "allow").await.unwrap();
    assert!(allows.iter().any(|(d, _, _)| d == "friend.example"));
    let blocks = policy::list(&pool, "block").await.unwrap();
    let spam = blocks.iter().find(|(d, _, _)| d == "spam.example").unwrap();
    assert_eq!(spam.2, "suspend");
    let noisy = blocks
        .iter()
        .find(|(d, _, _)| d == "noisy.example")
        .unwrap();
    assert_eq!(noisy.2, "silence");

    // Enforcement follows immediately (open posture): a suspended domain is
    // refused, a silenced one still federates but is filtered.
    assert!(
        !policy::domain_allowed(&pool, "us.example", "spam.example", false)
            .await
            .unwrap()
    );
    assert!(
        policy::domain_silenced(&pool, "noisy.example")
            .await
            .unwrap()
    );

    // Audited with the admin as actor.
    let actions: Vec<(String, String)> = audit::recent(&pool, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.action, e.target))
        .collect();
    assert!(actions.contains(&("fed_allow".into(), "friend.example".into())));
    assert!(actions.contains(&("fed_block".into(), "spam.example".into())));
    assert!(actions.contains(&("fed_block".into(), "noisy.example".into())));
}

#[tokio::test]
async fn d_removes_the_selected_entry() {
    let pool = setup().await;
    let sysop = admin(&pool).await;
    policy::set(&pool, "gone.example", "block", "", "suspend")
        .await
        .unwrap();

    let mut app = app_with(pool.clone(), sysop);
    app.screen = Screen::AdminUsers;
    press(&mut app, KeyCode::Char('f')).await;
    assert_eq!(app.fed_policy.len(), 1);
    press(&mut app, KeyCode::Char('d')).await;

    assert!(app.fed_policy.is_empty(), "list refreshed");
    assert!(policy::list(&pool, "block").await.unwrap().is_empty());
    let last = audit::recent(&pool, 5).await.unwrap();
    assert!(
        last.iter()
            .any(|e| e.action == "fed_remove" && e.target == "gone.example")
    );
}

#[tokio::test]
async fn an_empty_domain_is_refused_and_keeps_composing() {
    let pool = setup().await;
    let sysop = admin(&pool).await;
    let mut app = app_with(pool.clone(), sysop);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('f')).await;
    press(&mut app, KeyCode::Char('b')).await;
    press(&mut app, KeyCode::Enter).await; // nothing typed

    assert!(
        matches!(app.screen, Screen::ComposeFederation),
        "still composing"
    );
    assert!(policy::list(&pool, "block").await.unwrap().is_empty());
}

#[tokio::test]
async fn esc_cancels_the_domain_composer() {
    let pool = setup().await;
    let sysop = admin(&pool).await;
    let mut app = app_with(pool.clone(), sysop);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('f')).await;
    press(&mut app, KeyCode::Char('a')).await;
    typed(&mut app, "nope.example").await;
    press(&mut app, KeyCode::Esc).await;

    assert!(matches!(app.screen, Screen::AdminFederation));
    assert!(policy::list(&pool, "allow").await.unwrap().is_empty());
}
