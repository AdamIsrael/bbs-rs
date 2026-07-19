//! The multi-line compose editor, driven through the real App key handler (#96).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::{Field, Form, Screen};
use bbs_rs::config::Settings;
use bbs_rs::services::{self, auth, boards, presence::Presence};
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

async fn app_on_compose_post(pool: SqlitePool) -> App {
    let user = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let mut app = App::new(pool, Presence::new(), config(), user, 1, Transport::Ssh);
    // Put the app where `begin_compose_post` leaves it: a board selected, the
    // header holding the subject, the body empty and unfocused.
    app.current_board = Some(board);
    app.form = Form::new(vec![Field::new("Subject", false)]);
    app.body = bbs_rs::app::textarea::TextArea::new();
    app.body_focused = false;
    app.screen = Screen::ComposePost;
    app
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
async fn ctrl_d(app: &mut App) {
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await;
}

/// The headline capability: a body with several lines, entered with Enter for
/// newlines and sent with Ctrl-D, lands in the database intact.
#[tokio::test]
async fn a_multi_line_post_is_saved_with_its_line_breaks() {
    let pool = setup().await;
    let mut app = app_on_compose_post(pool.clone()).await;
    let board_id = app.current_board.as_ref().unwrap().id;

    // Subject in the header, then Tab into the body.
    typed(&mut app, "My subject").await;
    press(&mut app, KeyCode::Tab).await;
    assert!(
        app.body_focused,
        "Tab off the last header field enters the body"
    );

    typed(&mut app, "first line").await;
    press(&mut app, KeyCode::Enter).await; // newline, NOT submit
    typed(&mut app, "second line").await;
    assert_eq!(
        app.screen,
        Screen::ComposePost,
        "Enter in the body doesn't submit"
    );

    ctrl_d(&mut app).await;

    let posts = boards::list_messages(&pool, board_id).await.unwrap();
    let mine = posts.iter().find(|m| m.subject == "My subject").unwrap();
    assert_eq!(mine.body, "first line\nsecond line", "line break preserved");
    assert_eq!(
        app.screen,
        Screen::MessageList,
        "sending returns to the board"
    );
}

/// Backspace at the start of a body line joins it to the previous line — the
/// thing a single-line field could never do.
#[tokio::test]
async fn backspace_joins_body_lines() {
    let pool = setup().await;
    let mut app = app_on_compose_post(pool.clone()).await;
    let board_id = app.current_board.as_ref().unwrap().id;

    typed(&mut app, "Subj").await;
    press(&mut app, KeyCode::Tab).await;
    typed(&mut app, "one").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "two").await;
    // Cursor is at end of "two"; go home on that line and backspace to join.
    press(&mut app, KeyCode::Home).await;
    press(&mut app, KeyCode::Backspace).await;
    ctrl_d(&mut app).await;

    let posts = boards::list_messages(&pool, board_id).await.unwrap();
    let mine = posts.iter().find(|m| m.subject == "Subj").unwrap();
    assert_eq!(mine.body, "onetwo");
}

/// Focus moves back to the header from the top of the body, so a typo in the
/// subject is fixable without cancelling.
#[tokio::test]
async fn up_from_the_top_of_the_body_returns_to_the_header() {
    let pool = setup().await;
    let mut app = app_on_compose_post(pool).await;

    typed(&mut app, "wrong").await;
    press(&mut app, KeyCode::Tab).await;
    assert!(app.body_focused);
    // At row 0 of the body, Up steps back to the header.
    press(&mut app, KeyCode::Up).await;
    assert!(!app.body_focused, "back on the header");
    // Fix the subject.
    press(&mut app, KeyCode::Backspace).await;
    typed(&mut app, "3").await;
    assert_eq!(app.form.value(0), "wron3");
}

/// An empty body is allowed (a subject-only post), but an empty subject is not
/// — the same rule as before, still enforced.
#[tokio::test]
async fn an_empty_subject_is_refused() {
    let pool = setup().await;
    let mut app = app_on_compose_post(pool.clone()).await;
    let board_id = app.current_board.as_ref().unwrap().id;

    // No subject; go to the body, type, send.
    press(&mut app, KeyCode::Tab).await;
    typed(&mut app, "orphan body").await;
    ctrl_d(&mut app).await;

    assert_eq!(app.screen, Screen::ComposePost, "still composing");
    assert!(app.status.contains("Subject"), "told why: {}", app.status);
    assert!(
        boards::list_messages(&pool, board_id)
            .await
            .unwrap()
            .is_empty(),
        "nothing was posted"
    );
}

/// Esc cancels from either focus without posting.
#[tokio::test]
async fn esc_cancels_the_compose() {
    let pool = setup().await;
    let mut app = app_on_compose_post(pool.clone()).await;
    let board_id = app.current_board.as_ref().unwrap().id;

    typed(&mut app, "Subject").await;
    press(&mut app, KeyCode::Tab).await;
    typed(&mut app, "body text").await;
    press(&mut app, KeyCode::Esc).await;

    assert_eq!(app.screen, Screen::MessageList, "left the compose screen");
    assert!(
        boards::list_messages(&pool, board_id)
            .await
            .unwrap()
            .is_empty(),
        "nothing posted"
    );
}

/// The body respects the configured character limit, stopping input rather than
/// letting the post grow past what submit would reject.
#[tokio::test]
async fn the_body_stops_at_the_configured_limit() {
    let pool = setup().await;
    let user = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let mut settings = Settings::default();
    settings.limits.max_body_chars = 5;
    let mut app = App::new(
        pool,
        Presence::new(),
        Arc::new(settings),
        user,
        1,
        Transport::Ssh,
    );
    app.current_board = Some(board);
    app.form = Form::new(vec![Field::new("Subject", false)]);
    app.body = bbs_rs::app::textarea::TextArea::new();
    app.body_focused = true;
    app.screen = Screen::ComposePost;

    typed(&mut app, "abcdefghij").await; // 10 chars, limit is 5
    assert_eq!(app.body.text(), "abcde", "input stopped at the limit");
    assert!(app.status.contains("limit"), "said so: {}", app.status);
}

/// Mail compose has two header fields (recipient + subject) then the body, and
/// the multi-line body still reaches the mailbox intact.
#[tokio::test]
async fn a_multi_line_mail_is_sent() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let mut app = App::new(
        pool.clone(),
        Presence::new(),
        config(),
        alice,
        1,
        Transport::Ssh,
    );
    app.form = Form::new(vec![
        Field::new("To (username)", false),
        Field::new("Subject", false),
    ]);
    app.body = bbs_rs::app::textarea::TextArea::new();
    app.body_focused = false;
    app.screen = Screen::ComposeMail;

    typed(&mut app, "bob").await;
    press(&mut app, KeyCode::Tab).await; // -> subject
    typed(&mut app, "Hello").await;
    press(&mut app, KeyCode::Tab).await; // -> body
    assert!(app.body_focused);
    typed(&mut app, "line one").await;
    press(&mut app, KeyCode::Enter).await;
    typed(&mut app, "line two").await;
    ctrl_d(&mut app).await;

    let bob = auth::find_user(&pool, "bob").await.unwrap().unwrap();
    let inbox = bbs_rs::services::mail::inbox(&pool, bob.id).await.unwrap();
    let msg = inbox.iter().find(|m| m.subject == "Hello").unwrap();
    assert_eq!(msg.body, "line one\nline two");
    assert_eq!(
        app.screen,
        Screen::Mailbox,
        "sending returns to the mailbox"
    );
}
