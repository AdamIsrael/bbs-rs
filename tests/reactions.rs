//! Post reactions (#94) driven through the real App key handler: opening a post
//! loads its reaction state, and the digit keys toggle the viewer's reaction.
//! The SQL-level behaviour is covered in `tests/services.rs`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::app::ui;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, boards, presence::Presence};
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

async fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .await;
}

/// An app opened onto a single post's ReadMessage screen, as `user`.
async fn app_reading_a_post(pool: SqlitePool, user: User, author: &User) -> (App, i64) {
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        author,
        "Subj",
        "body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    let mut app = App::new(
        pool,
        Presence::new(),
        Arc::new(Settings::default()),
        user,
        1,
        Transport::Ssh,
    );
    app.current_board = Some(board);
    // Load the thread list through the app's own pool, then drive the real
    // open path: select the post and press Enter.
    app.reload_messages_for_test().await;
    app.msg_sel = 0;
    app.screen = Screen::MessageList;
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    (app, id)
}

fn up_count(app: &App) -> i64 {
    app.current_msg_reactions
        .iter()
        .find(|(k, _)| *k == "up")
        .map(|(_, n)| *n)
        .unwrap_or(-1)
}

#[tokio::test]
async fn digit_key_toggles_a_reaction_and_updates_counts() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, id) = app_reading_a_post(pool, alice.clone(), &alice).await;
    assert!(matches!(app.screen, Screen::ReadMessage));
    assert_eq!(up_count(&app), 0, "no reactions yet");

    // Press '1' → the first palette reaction (up).
    press(&mut app, '1').await;
    assert_eq!(up_count(&app), 1, "reaction added");
    assert!(app.current_msg_my_reactions.contains("up"), "shown as mine");
    assert_eq!(
        app.msg_reaction_totals.get(&id),
        Some(&1),
        "the list badge total tracks it"
    );

    // Press '1' again → toggled off.
    press(&mut app, '1').await;
    assert_eq!(up_count(&app), 0, "reaction removed");
    assert!(!app.current_msg_my_reactions.contains("up"));
    assert_eq!(app.msg_reaction_totals.get(&id), None, "badge cleared");
}

#[tokio::test]
async fn the_reader_renders_the_reaction_footer() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _id) = app_reading_a_post(pool, alice.clone(), &alice).await;
    press(&mut app, '1').await; // react so a count shows

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| ui::draw(f, &app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();

    assert!(screen.contains("Reactions:"), "the footer is drawn");
    assert!(screen.contains("×1"), "the reacted count shows: {screen}");
    assert!(
        screen.contains("1-3 react"),
        "the status bar hints the keys"
    );
}

#[tokio::test]
async fn a_guest_cannot_react() {
    let pool = setup().await;
    let author = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let (mut app, _id) = app_reading_a_post(pool, guest, &author).await;

    press(&mut app, '1').await;
    assert_eq!(up_count(&app), 0, "guest's press adds nothing");
    assert!(
        app.status.contains("register"),
        "guest is nudged to register, got: {:?}",
        app.status
    );
}
