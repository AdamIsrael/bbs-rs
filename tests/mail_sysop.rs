//! Mail to sysop (#71): a feedback path that resolves the primary admin and
//! reuses the mail service, available even when private mail is off.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, admin, auth, mail, presence::Presence};
use bbs_rs::transport::Transport;

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

async fn reg(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap()
}

/// Register the primary admin (lowest id) plus a second admin.
async fn make_admins(pool: &SqlitePool) {
    reg(pool, "boss").await;
    admin::set_role(pool, "boss", "admin").await.unwrap();
    reg(pool, "deputy").await;
    admin::set_role(pool, "deputy", "admin").await.unwrap();
}

fn app(pool: SqlitePool, settings: Settings, user: User) -> App {
    App::new(
        pool,
        Presence::new(),
        Arc::new(settings),
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
async fn primary_admin_is_the_lowest_id_admin() {
    let pool = setup().await;
    assert!(
        admin::primary_admin(&pool).await.unwrap().is_none(),
        "none yet"
    );
    make_admins(&pool).await;
    assert_eq!(
        admin::primary_admin(&pool).await.unwrap().unwrap().username,
        "boss",
        "the first admin registered"
    );
}

#[tokio::test]
async fn a_user_can_mail_the_sysop_even_with_private_mail_off() {
    let pool = setup().await;
    make_admins(&pool).await;
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();
    let alice = reg(&pool, "alice").await;

    // Private mail is OFF — but the sysop feedback path still works.
    let mut settings = Settings::default();
    settings.features.private_mail = false;
    let mut app = app(pool.clone(), settings, alice.clone());

    // 'e' opens the sysop composer, addressed to the primary admin.
    press(&mut app, KeyCode::Char('e')).await;
    assert!(matches!(app.screen, Screen::MailSysop));

    typed(&mut app, "Please add a chess door").await;
    press(&mut app, KeyCode::Down).await; // into the body
    typed(&mut app, "It would be great.").await;
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await; // ^D sends

    assert!(
        matches!(app.screen, Screen::MainMenu),
        "returns to the menu"
    );
    let inbox = mail::inbox(&pool, boss.id).await.unwrap();
    assert_eq!(inbox.len(), 1, "the sysop got the feedback");
    assert_eq!(inbox[0].subject, "Please add a chess door");
    assert_eq!(inbox[0].from_name, "alice");
}

#[tokio::test]
async fn a_guest_is_asked_to_register() {
    let pool = setup().await;
    make_admins(&pool).await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let mut app = app(pool, Settings::default(), guest);

    press(&mut app, KeyCode::Char('e')).await;
    assert!(
        matches!(app.screen, Screen::MainMenu),
        "composer not opened"
    );
    assert!(
        app.status.contains("Register"),
        "guest nudged: {:?}",
        app.status
    );
}

#[tokio::test]
async fn with_no_admin_there_is_nobody_to_write_to() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let mut app = app(pool, Settings::default(), alice);

    press(&mut app, KeyCode::Char('e')).await;
    assert!(matches!(app.screen, Screen::MainMenu));
    assert!(app.status.contains("no sysop"), "got: {:?}", app.status);
}
