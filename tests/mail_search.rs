//! Mailbox full-text search (#93), driven through the real App key handler:
//! `/` on the Mailbox opens a query prompt, Enter shows matching mail, and
//! Enter on a hit opens it.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, mail, presence::Presence};
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

#[tokio::test]
async fn slash_searches_the_mailbox_and_opens_a_hit() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    mail::send_mail(
        &pool,
        &bob,
        "alice",
        "Dinner plans",
        "thursday at 7",
        &Default::default(),
    )
    .await
    .unwrap();
    mail::send_mail(
        &pool,
        &bob,
        "alice",
        "Invoice",
        "attached pdf",
        &Default::default(),
    )
    .await
    .unwrap();

    let mut app = app_with(pool.clone(), alice);
    app.screen = Screen::Mailbox;

    // `/` -> query prompt.
    press(&mut app, KeyCode::Char('/')).await;
    assert!(matches!(app.screen, Screen::MailSearchInput));

    typed(&mut app, "dinner").await;
    press(&mut app, KeyCode::Enter).await;

    assert!(matches!(app.screen, Screen::MailSearchResults));
    assert_eq!(app.mail_search.len(), 1, "only the dinner mail matched");
    assert_eq!(app.mail_search[0].subject, "Dinner plans");
    assert_eq!(app.mail_search_query, "dinner");

    // Enter opens the hit.
    press(&mut app, KeyCode::Enter).await;
    assert!(matches!(app.screen, Screen::ReadMail));
    assert_eq!(app.current_mail.as_ref().unwrap().subject, "Dinner plans");
}

#[tokio::test]
async fn esc_from_the_query_returns_to_the_mailbox() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let mut app = app_with(pool, alice);
    app.screen = Screen::Mailbox;

    press(&mut app, KeyCode::Char('/')).await;
    typed(&mut app, "whatever").await;
    press(&mut app, KeyCode::Esc).await;
    assert!(matches!(app.screen, Screen::Mailbox));
}
