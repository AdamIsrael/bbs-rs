//! The main menu should offer "Register New Account" only to the guest account
//! (the newcomer bootstrap path); registered users must not see it.

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use sshtui::app::state::MenuItem;
use sshtui::app::App;
use sshtui::services::{self, auth, presence::Presence};

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
    services::seed(&pool).await.expect("seed");
    pool
}

#[tokio::test]
async fn guest_sees_register() {
    let pool = setup().await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let app = App::new(pool, Presence::new(), guest, 1);
    assert!(
        app.menu.contains(&MenuItem::Register),
        "guest should see the Register option"
    );
}

#[tokio::test]
async fn registered_user_does_not_see_register() {
    let pool = setup().await;
    let user = auth::register_user(&pool, "alice", "pw").await.unwrap();
    let app = App::new(pool, Presence::new(), user, 1);
    assert!(
        !app.menu.contains(&MenuItem::Register),
        "registered users should not see the Register option"
    );
    // ...but the rest of the menu is intact.
    assert!(app.menu.contains(&MenuItem::Boards));
    assert!(app.menu.contains(&MenuItem::Mail));
    assert!(app.menu.contains(&MenuItem::Quit));
}
