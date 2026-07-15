//! The main menu should offer "Register New Account" only to the guest account
//! (the newcomer bootstrap path); registered users must not see it.

use std::sync::Arc;

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::MenuItem;
use bbs_rs::config::Settings;
use bbs_rs::services::{self, auth, presence::Presence};

fn config() -> Arc<Settings> {
    Arc::new(Settings::default())
}

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

#[tokio::test]
async fn guest_sees_register() {
    let pool = setup().await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let app = App::new(pool, Presence::new(), config(), guest, 1);
    assert!(
        app.menu.contains(&MenuItem::Register),
        "guest should see the Register option"
    );
}

#[tokio::test]
async fn registered_user_does_not_see_register() {
    let pool = setup().await;
    let user = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let app = App::new(pool, Presence::new(), config(), user, 1);
    assert!(
        !app.menu.contains(&MenuItem::Register),
        "registered users should not see the Register option"
    );
    // ...but the rest of the menu is intact.
    assert!(app.menu.contains(&MenuItem::Boards));
    assert!(app.menu.contains(&MenuItem::Mail));
    assert!(app.menu.contains(&MenuItem::Quit));
}

#[tokio::test]
async fn oneliners_menu_follows_feature_toggle() {
    let pool = setup().await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();

    // On by default.
    let app = App::new(pool.clone(), Presence::new(), config(), guest.clone(), 1);
    assert!(app.menu.contains(&MenuItem::Oneliners));

    // Disabling the feature removes the menu item.
    let mut settings = Settings::default();
    settings.features.oneliners = false;
    let app = App::new(pool, Presence::new(), Arc::new(settings), guest, 2);
    assert!(!app.menu.contains(&MenuItem::Oneliners));
}
