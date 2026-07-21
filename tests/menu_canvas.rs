//! ANSI menu canvas (#85): when a main-menu backdrop is configured and every
//! entry is placed, the menu renders labels at those coordinates over the art;
//! otherwise it falls back to the bordered list.

use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Text;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::state::{MenuEntry, MenuItem, Screen};
use bbs_rs::app::{App, ui};
use bbs_rs::config::Settings;
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

async fn base_app() -> App {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    App::new(
        pool,
        Presence::new(),
        Arc::new(Settings::default()),
        alice,
        1,
        Transport::Ssh,
    )
}

fn entry(item: MenuItem, label: &str, row: Option<u16>, col: Option<u16>) -> MenuEntry {
    MenuEntry {
        item,
        label: label.into(),
        key: Some(item.default_key()),
        row,
        col,
    }
}

/// Read `len` characters from row `y`, starting at column `x`, of a rendered
/// TestBackend buffer.
fn cell_text(term: &Terminal<TestBackend>, x: u16, y: u16, len: u16) -> String {
    let buf = term.backend().buffer();
    (0..len)
        .map(|i| buf[(x + i, y)].symbol())
        .collect::<String>()
}

fn dump(term: &Terminal<TestBackend>) -> String {
    term.backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

fn render(app: &App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    term
}

#[tokio::test]
async fn a_placed_menu_draws_labels_at_their_coordinates() {
    let mut app = base_app().await;
    // A blank backdrop so labels land on empty space.
    let canvas = Text::from("\n".repeat(11)); // 12 blank rows
    app.art.insert(Screen::MainMenu, canvas);
    app.menu = vec![
        entry(MenuItem::Boards, "BOARDS", Some(1), Some(4)),
        entry(MenuItem::Quit, "LOGOFF", Some(3), Some(4)),
    ];
    app.menu_sel = 0;

    let term = render(&app, 40, 14);
    // Body starts one row below the title bar, so config row R lands on screen
    // row R+1, at the configured column.
    assert_eq!(
        cell_text(&term, 4, 2, 6),
        "BOARDS",
        "placed at (row 1, col 4)"
    );
    assert_eq!(
        cell_text(&term, 4, 4, 6),
        "LOGOFF",
        "placed at (row 3, col 4)"
    );
    // Canvas layout shows no "[k]" list hotkey brackets and no list border.
    let all = dump(&term);
    assert!(!all.contains("[b]"), "canvas has no list hotkey brackets");
}

#[tokio::test]
async fn a_partial_layout_falls_back_to_the_list() {
    let mut app = base_app().await;
    // A short backdrop so the fallback list still has room to render.
    app.art.insert(Screen::MainMenu, Text::from("art"));
    // One entry placed, one not → not all placed → list layout.
    app.menu = vec![
        entry(MenuItem::Boards, "Boards", Some(1), Some(4)),
        entry(MenuItem::Quit, "Quit", None, None),
    ];

    let all = dump(&render(&app, 40, 14));
    assert!(
        all.contains("[b]"),
        "falls back to the list (hotkey brackets shown)"
    );
}

#[tokio::test]
async fn no_backdrop_means_the_list_even_when_placed() {
    let mut app = base_app().await;
    // Placed entries but no MainMenu art → list.
    app.menu = vec![
        entry(MenuItem::Boards, "Boards", Some(1), Some(4)),
        entry(MenuItem::Quit, "Quit", Some(3), Some(4)),
    ];
    let all = dump(&render(&app, 40, 14));
    assert!(all.contains("[b]"), "no backdrop → list");
}

#[tokio::test]
async fn a_placement_that_does_not_fit_falls_back_to_the_list() {
    let mut app = base_app().await;
    app.art.insert(Screen::MainMenu, Text::from("art"));
    // Placed at row 40 — off a short terminal, so the canvas can't fit.
    app.menu = vec![
        entry(MenuItem::Boards, "Boards", Some(40), Some(2)),
        entry(MenuItem::Quit, "Quit", Some(42), Some(2)),
    ];
    let all = dump(&render(&app, 40, 14));
    assert!(
        all.contains("[b]"),
        "off-screen placement falls back to the list"
    );
}
