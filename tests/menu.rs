//! Config-driven main menu (#84): the built-in default when `[[menu]]` is
//! empty, an operator-defined menu (order / label / hotkey) when it isn't, with
//! feature and role gating either way, plus letter-hotkey dispatch.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::{MenuAction, MenuItem, Screen};
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
        row: None,
        col: None,
    }
}

#[tokio::test]
async fn the_default_menu_is_the_classic_set_with_hotkeys() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let app = app(pool, Settings::default(), alice);

    // Order preserved; boards is second, after bulletins.
    assert_eq!(app.menu[0].action, MenuAction::Builtin(MenuItem::Bulletins));
    assert_eq!(app.menu[1].action, MenuAction::Builtin(MenuItem::Boards));
    // Default label + default hotkey.
    assert_eq!(app.menu[1].label, "Message Boards");
    assert_eq!(app.menu[1].key, Some('b'));
    // Quit is present at the end with its default key.
    let quit = app.menu.last().unwrap();
    assert_eq!(quit.action, MenuAction::Builtin(MenuItem::Quit));
    assert_eq!(quit.key, Some('q'));
    // A guest-only item is absent for a registered user, and admin too.
    assert!(
        !app.menu
            .iter()
            .any(|e| e.action == MenuAction::Builtin(MenuItem::Register))
    );
    assert!(
        !app.menu
            .iter()
            .any(|e| e.action == MenuAction::Builtin(MenuItem::Admin))
    );
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
    assert_eq!(app.menu[0].action, MenuAction::Builtin(MenuItem::Who));
    assert_eq!(app.menu[1].action, MenuAction::Builtin(MenuItem::Boards));
    assert_eq!(app.menu[1].label, "Message Bases");
    assert_eq!(app.menu[1].key, Some('m'));
    assert_eq!(app.menu[2].action, MenuAction::Builtin(MenuItem::Quit));
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

    let items: Vec<MenuAction> = app.menu.iter().map(|e| e.action.clone()).collect();
    assert_eq!(
        items,
        vec![MenuAction::Builtin(MenuItem::Boards)],
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

// ---- #86: compound targets & submenus -----------------------------------

#[test]
fn compound_actions_parse_into_their_targets() {
    assert_eq!(
        MenuAction::parse("boards"),
        Some(MenuAction::Builtin(MenuItem::Boards))
    );
    assert_eq!(
        MenuAction::parse("door:lord"),
        Some(MenuAction::Door("lord".into()))
    );
    assert_eq!(
        MenuAction::parse("board:General"),
        Some(MenuAction::Board("General".into()))
    );
    assert_eq!(
        MenuAction::parse(" submenu: games "),
        Some(MenuAction::Submenu("games".into())),
        "prefix and name are trimmed"
    );
    // An empty compound name and an unknown built-in both fail to parse.
    assert_eq!(MenuAction::parse("door:"), None);
    assert_eq!(MenuAction::parse("bogus"), None);
}

fn door(name: &str) -> bbs_rs::config::Door {
    bbs_rs::config::Door {
        name: name.into(),
        command: "true".into(),
        args: vec![],
        cwd: None,
        time_limit_secs: 0,
        drop_file: None,
    }
}

#[tokio::test]
async fn a_submenu_pushes_and_esc_pops_back() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let mut submenus = std::collections::HashMap::new();
    submenus.insert(
        "games".to_string(),
        vec![entry("who", "", ""), entry("quit", "", "")],
    );
    let settings = Settings {
        menu: vec![
            entry("submenu:games", "Game Room", ""),
            entry("quit", "", ""),
        ],
        submenus,
        ..Default::default()
    };
    let mut app = app(pool, settings, alice);

    // Top level: the submenu entry and Quit.
    assert_eq!(app.menu.len(), 2);
    assert_eq!(app.menu[0].action, MenuAction::Submenu("games".into()));
    assert_eq!(app.menu[0].key, Some('g'), "default key = first letter");

    // Enter descends into the submenu.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    assert!(matches!(app.screen, Screen::MainMenu), "still on the menu");
    assert_eq!(app.menu_title.as_deref(), Some("Game Room"));
    assert_eq!(app.menu_stack.len(), 1);
    let items: Vec<MenuAction> = app.menu.iter().map(|e| e.action.clone()).collect();
    assert_eq!(
        items,
        vec![
            MenuAction::Builtin(MenuItem::Who),
            MenuAction::Builtin(MenuItem::Quit)
        ]
    );

    // Esc pops back to the top level, restoring its selection and title.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await;
    assert!(app.menu_stack.is_empty());
    assert_eq!(app.menu_title, None);
    assert_eq!(app.menu[0].action, MenuAction::Submenu("games".into()));
    assert!(!app.should_quit, "Esc in a submenu pops, it does not quit");
}

#[tokio::test]
async fn a_door_target_launches_that_door() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let settings = Settings {
        menu: vec![entry("door:lord", "Play LORD", "")],
        doors: vec![door("lord")],
        ..Default::default()
    };
    let mut app = app(pool, settings, alice);

    assert_eq!(app.menu.len(), 1);
    assert_eq!(app.menu[0].action, MenuAction::Door("lord".into()));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    assert_eq!(
        app.pending_door,
        Some(0),
        "the run loop is signalled to launch doors[0]"
    );
}

#[tokio::test]
async fn a_board_target_opens_that_board() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let settings = Settings {
        menu: vec![entry("board:General", "", "")],
        ..Default::default()
    };
    let mut app = app(pool, settings, alice);

    assert_eq!(app.menu[0].action, MenuAction::Board("General".into()));
    assert_eq!(app.menu[0].label, "General", "blank label defaults to name");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    assert!(
        matches!(app.screen, Screen::MessageList),
        "the board opened directly"
    );
}

#[tokio::test]
async fn dangling_door_and_submenu_targets_are_dropped() {
    let pool = setup().await;
    let alice = user(&pool, "alice").await;
    let settings = Settings {
        // Neither the door nor the submenu is configured → both dropped.
        menu: vec![
            entry("door:ghost", "", ""),
            entry("submenu:missing", "", ""),
            entry("quit", "", ""),
        ],
        ..Default::default()
    };
    let app = app(pool, settings, alice);

    let items: Vec<MenuAction> = app.menu.iter().map(|e| e.action.clone()).collect();
    assert_eq!(
        items,
        vec![MenuAction::Builtin(MenuItem::Quit)],
        "only the resolvable entry survives"
    );
}
