//! In-BBS sysop broadcast (#69), driven through the real App key handler: from
//! the admin users screen, `w` opens a one-line composer and Enter fans the
//! message out to every live session via the presence registry.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, presence::Presence};
use bbs_rs::transport::{Event, Transport};

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

async fn admin_user(pool: &SqlitePool) -> User {
    auth::register_user(pool, "sysop", "pw", &Default::default())
        .await
        .unwrap();
    services::admin::set_role(pool, "sysop", "admin")
        .await
        .unwrap();
    // Re-fetch so the in-memory role reflects the promotion.
    auth::find_user(pool, "sysop").await.unwrap().unwrap()
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
async fn w_opens_the_broadcast_composer() {
    let pool = setup().await;
    let admin = admin_user(&pool).await;
    let presence = Presence::new();
    let mut app = App::new(pool, presence, config(), admin, 1, Transport::Ssh);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('w')).await;
    assert!(matches!(app.screen, Screen::ComposeBroadcast));
}

#[tokio::test]
async fn sending_a_broadcast_fans_out_to_live_sessions() {
    let pool = setup().await;
    let admin = admin_user(&pool).await;
    let presence = Presence::new();
    let (tx, mut rx) = mpsc::channel(4);
    presence.join(42, "listener".into(), None, tx).await;
    let mut app = App::new(pool, presence, config(), admin, 1, Transport::Ssh);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('w')).await;
    typed(&mut app, "maintenance in 5").await;
    press(&mut app, KeyCode::Enter).await;

    assert!(matches!(app.screen, Screen::AdminUsers), "back to admin");
    assert!(
        app.status.starts_with("Broadcast reached"),
        "{}",
        app.status
    );
    match rx.try_recv() {
        Ok(Event::Broadcast { text }) => assert_eq!(text, "maintenance in 5"),
        other => panic!("listener should have received the broadcast, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_broadcast_is_refused() {
    let pool = setup().await;
    let admin = admin_user(&pool).await;
    let presence = Presence::new();
    let (tx, mut rx) = mpsc::channel(4);
    presence.join(42, "listener".into(), None, tx).await;
    let mut app = App::new(pool, presence, config(), admin, 1, Transport::Ssh);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('w')).await;
    press(&mut app, KeyCode::Enter).await; // nothing typed

    assert_eq!(app.status, "Nothing to broadcast.");
    assert!(rx.try_recv().is_err(), "no empty broadcast delivered");
}

#[tokio::test]
async fn esc_cancels_a_broadcast() {
    let pool = setup().await;
    let admin = admin_user(&pool).await;
    let presence = Presence::new();
    let (tx, mut rx) = mpsc::channel(4);
    presence.join(42, "listener".into(), None, tx).await;
    let mut app = App::new(pool, presence, config(), admin, 1, Transport::Ssh);
    app.screen = Screen::AdminUsers;

    press(&mut app, KeyCode::Char('w')).await;
    typed(&mut app, "oops").await;
    press(&mut app, KeyCode::Esc).await;

    assert!(matches!(app.screen, Screen::AdminUsers));
    assert!(
        rx.try_recv().is_err(),
        "a cancelled broadcast is never sent"
    );
}
