//! Polls (#72) driven through the real App key handler: listing, opening,
//! voting with digit keys, and creating via the compose form. SQL-level
//! behaviour is covered in `tests/services.rs`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::state::Screen;
use bbs_rs::app::{App, ui};
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, polls, presence::Presence};
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

fn app_for(pool: SqlitePool, user: User) -> App {
    App::new(
        pool,
        Presence::new(),
        Arc::new(Settings::default()),
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
async fn open_a_poll_and_vote_with_a_digit_key() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let id = polls::create_poll(
        &pool,
        &alice,
        "Best editor?",
        &["vim".into(), "emacs".into()],
        &Default::default(),
        1,
    )
    .await
    .unwrap();

    let mut app = app_for(pool, alice);
    // Menu → Polls (hotkey 'v'), then Enter into the poll.
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
        .await;
    assert!(matches!(app.screen, Screen::Polls));
    press(&mut app, KeyCode::Enter).await;
    assert!(matches!(app.screen, Screen::ViewPoll));

    // Vote for option 2 (emacs) via the digit key.
    press(&mut app, KeyCode::Char('2')).await;
    let detail = app.current_poll.as_ref().unwrap();
    assert_eq!(detail.poll.total_votes, 1);
    assert_eq!(detail.options[1].votes, 1, "emacs got the vote");
    assert_eq!(detail.my_vote, Some(detail.options[1].id));

    // Re-vote for option 1 — still one vote, moved.
    press(&mut app, KeyCode::Char('1')).await;
    let detail = app.current_poll.as_ref().unwrap();
    assert_eq!(detail.poll.total_votes, 1);
    assert_eq!(detail.options[0].votes, 1);
    assert_eq!(detail.my_vote, Some(detail.options[0].id));

    let _ = id;
}

#[tokio::test]
async fn create_a_poll_through_the_compose_form() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let mut app = app_for(pool.clone(), alice);

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
        .await;
    press(&mut app, KeyCode::Char('n')).await; // new poll
    assert!(matches!(app.screen, Screen::ComposePoll));

    typed(&mut app, "Tabs or spaces?").await;
    press(&mut app, KeyCode::Tab).await; // → Option 1
    typed(&mut app, "tabs").await;
    press(&mut app, KeyCode::Tab).await; // → Option 2
    typed(&mut app, "spaces").await;
    // Jump to the last field and submit from there.
    while !app.form.on_last() {
        press(&mut app, KeyCode::Tab).await;
    }
    press(&mut app, KeyCode::Enter).await;

    // Landed on the new poll's view.
    assert!(matches!(app.screen, Screen::ViewPoll));
    let detail = app.current_poll.as_ref().unwrap();
    assert_eq!(detail.poll.question, "Tabs or spaces?");
    assert_eq!(detail.options.len(), 2);
    // And it's persisted.
    assert_eq!(polls::list_polls(&pool, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_guest_cannot_create_a_poll() {
    let pool = setup().await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let mut app = app_for(pool, guest);

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
        .await;
    press(&mut app, KeyCode::Char('n')).await;
    assert!(matches!(app.screen, Screen::Polls), "compose not opened");
    assert!(
        app.status.contains("register"),
        "guest nudged: {:?}",
        app.status
    );
}

#[tokio::test]
async fn the_poll_view_renders_a_result_bar() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    polls::create_poll(
        &pool,
        &alice,
        "Best editor?",
        &["vim".into(), "emacs".into()],
        &Default::default(),
        1,
    )
    .await
    .unwrap();
    let mut app = app_for(pool, alice);
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
        .await;
    press(&mut app, KeyCode::Enter).await;
    press(&mut app, KeyCode::Char('1')).await; // vote vim

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| ui::draw(f, &app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();

    assert!(screen.contains("Best editor?"), "question shown");
    assert!(screen.contains("vim"), "option shown");
    assert!(
        screen.contains("100%"),
        "the sole vote reads as 100%: {screen}"
    );
}
