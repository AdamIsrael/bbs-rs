//! New-user validation queue (#73) through the real App key handler: an admin
//! approves/rejects a pending registration from the Admin · Users screen.
//! Service-level behaviour is covered in `tests/services.rs`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::{Accounts, Settings};
use bbs_rs::db::models::User;
use bbs_rs::services::{self, admin, auth, presence::Presence};
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

fn requiring() -> Accounts {
    Accounts {
        require_validation: true,
        ..Default::default()
    }
}

/// A registered admin.
async fn admin_user(pool: &SqlitePool) -> User {
    auth::register_user(pool, "boss", "pw", &Default::default())
        .await
        .unwrap();
    admin::set_role(pool, "boss", "admin").await.unwrap();
    auth::find_user(pool, "boss").await.unwrap().unwrap()
}

async fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .await;
}

/// Open the admin users screen and point the cursor at `username`.
async fn admin_on_user(pool: SqlitePool, admin: User, username: &str) -> App {
    let mut app = App::new(
        pool,
        Presence::new(),
        Arc::new(Settings::default()),
        admin,
        1,
        Transport::Ssh,
    );
    // 'a' from the main menu opens Admin · Users.
    press(&mut app, 'a').await;
    assert!(matches!(app.screen, Screen::AdminUsers));
    app.admin_user_sel = app
        .admin_users
        .iter()
        .position(|u| u.username == username)
        .expect("target user in the admin list");
    app
}

#[tokio::test]
async fn approving_a_pending_user_lets_them_log_in() {
    let pool = setup().await;
    auth::register_user(&pool, "newbie", "pw", &requiring())
        .await
        .unwrap();
    let admin = admin_user(&pool).await;
    // Sanity: newbie can't log in yet.
    assert!(
        auth::attempt_login(&pool, "newbie", "pw", None)
            .await
            .unwrap()
            .is_none()
    );

    let mut app = admin_on_user(pool.clone(), admin, "newbie").await;
    press(&mut app, 'v').await; // approve

    assert!(
        auth::attempt_login(&pool, "newbie", "pw", None)
            .await
            .unwrap()
            .is_some(),
        "approved user can now log in"
    );
    // The list refreshed and no longer shows them pending.
    let row = app
        .admin_users
        .iter()
        .find(|u| u.username == "newbie")
        .unwrap();
    assert!(row.is_validated());
}

#[tokio::test]
async fn rejecting_a_pending_user_removes_them() {
    let pool = setup().await;
    auth::register_user(&pool, "spammer", "pw", &requiring())
        .await
        .unwrap();
    let admin = admin_user(&pool).await;

    let mut app = admin_on_user(pool.clone(), admin, "spammer").await;
    press(&mut app, 'x').await; // reject

    assert!(auth::find_user(&pool, "spammer").await.unwrap().is_none());
    assert!(!app.admin_users.iter().any(|u| u.username == "spammer"));
}
