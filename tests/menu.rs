//! Config-driven main menu (#84): the built-in default when `[[menu]]` is
//! empty, an operator-defined menu (order / label / hotkey) when it isn't, with
//! feature and role gating either way, plus letter-hotkey dispatch.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::{MenuItem, Screen};
use bbs_rs::config::{MenuEntry, Settings};
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, presence::Presence};
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

async fn user(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap()
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

fn entry(action: &str, label: &str, key: &str) -> MenuEntry {
    MenuEntry {
        action: action.into(),
        label: label.into(),
        key: key.into(),
    }
}

#[tokio::test]
async fn the_default_menu_is_the_classic_set_with_hotkeys() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let app = app(pool, Settings::default(), alice);

    // Order preserved; boards is second, after bulletins.
    assert_eq!(app.menu[0].item, MenuItem::Bulletins);
    assert_eq!(app.menu[1].item, MenuItem::Boards);
    // Default label + default hotkey.
    assert_eq!(app.menu[1].label, "Message Boards");
    assert_eq!(app.menu[1].key, Some('b'));
    // Quit is present at the end with its default key.
    let quit = app.menu.last().unwrap();
    assert_eq!(quit.item, MenuItem::Quit);
    assert_eq!(quit.key, Some('q'));
    // A guest-only item is absent for a registered user, and admin too.
    assert!(!app.menu.iter().any(|e| e.item == MenuItem::Register));
    assert!(!app.menu.iter().any(|e| e.item == MenuItem::Admin));
}

#[tokio::test]
async fn a_configured_menu_sets_order_label_and_hotkey() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let settings = Settings {
        menu: vec![
            entry("who", "", ""),                  // defaults
            entry("boards", "Message Bases", "m"), // custom label + key
            entry("quit", "Log off", ""),          // custom label, default key
            entry("bogus", "", ""),                // unknown action → dropped
        ],
        ..Default::default()
    };
    let app = app(pool, settings, alice);

    assert_eq!(app.menu.len(), 3, "the unknown action was dropped");
    assert_eq!(app.menu[0].item, MenuItem::Who);
    assert_eq!(app.menu[1].item, MenuItem::Boards);
    assert_eq!(app.menu[1].label, "Message Bases");
    assert_eq!(app.menu[1].key, Some('m'));
    assert_eq!(app.menu[2].item, MenuItem::Quit);
    assert_eq!(app.menu[2].label, "Log off");
    assert_eq!(
        app.menu[2].key,
        Some('q'),
        "blank key falls back to default"
    );
}

#[tokio::test]
async fn feature_and_role_gates_still_drop_configured_entries() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let mut settings = Settings {
        menu: vec![
            entry("mail", "", ""),
            entry("admin", "", ""),
            entry("boards", "", ""),
        ],
        ..Default::default()
    };
    settings.features.private_mail = false; // mail off
    let app = app(pool, settings, alice); // alice is not an admin

    let items: Vec<MenuItem> = app.menu.iter().map(|e| e.item).collect();
    assert_eq!(
        items,
        vec![MenuItem::Boards],
        "mail (off) and admin (role) dropped"
    );
}

#[tokio::test]
async fn a_hotkey_activates_its_entry() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let mut app = app(pool, Settings::default(), alice);
    assert!(matches!(app.screen, Screen::MainMenu));

    // 'b' -> Boards.
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
        .await;
    assert!(
        matches!(app.screen, Screen::BoardList),
        "hotkey opened boards"
    );

    // An unbound letter does nothing.
    app.screen = Screen::MainMenu;
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))
        .await;
    assert!(matches!(app.screen, Screen::MainMenu));
}

#[test]
fn actions_round_trip_and_reject_typos() {
    for item in [
        MenuItem::Boards,
        MenuItem::Mail,
        MenuItem::Quit,
        MenuItem::Admin,
    ] {
        assert_eq!(MenuItem::from_action(item.action()), Some(item));
    }
    // Case-insensitive and trimmed.
    assert_eq!(MenuItem::from_action("  BOARDS "), Some(MenuItem::Boards));
    assert_eq!(MenuItem::from_action("nope"), None);
}
