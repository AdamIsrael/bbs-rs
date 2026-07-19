//! Author edit/delete of own board posts, driven through the real App key
//! handler (#92). These prove the keystroke wiring — that `e`/`d` reach the
//! author-scoped service paths and that the composer round-trips an edit — on
//! top of the SQL-scoping tests in `tests/services.rs`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
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

/// An app sitting on a board's message list as `user`, with one post already
/// selected (the one just made). Returns the app and the post id.
async fn app_on_list(pool: SqlitePool, user: User, author: &User) -> (App, SqlitePool, i64) {
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        author,
        "Subject",
        "original body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    let messages = boards::list_thread(&pool, board.id).await.unwrap();
    let mut app = App::new(
        pool.clone(),
        Presence::new(),
        config(),
        user,
        1,
        Transport::Ssh,
    );
    app.current_board = Some(board);
    app.messages = messages;
    app.msg_sel = 0;
    app.screen = Screen::MessageList;
    (app, pool, id)
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

#[tokio::test]
async fn e_on_the_list_opens_the_editor_prefilled() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _pool, _id) = app_on_list(pool, alice.clone(), &alice).await;

    press(&mut app, KeyCode::Char('e')).await;

    assert!(matches!(app.screen, Screen::ComposePost));
    assert!(app.is_editing_post(), "the composer knows it's an edit");
    assert_eq!(app.form.value(0), "Subject", "subject prefilled");
    assert_eq!(app.body.text(), "original body", "body prefilled");
}

#[tokio::test]
async fn editing_a_post_updates_it_and_returns_to_the_list() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, pool, id) = app_on_list(pool, alice.clone(), &alice).await;

    press(&mut app, KeyCode::Char('e')).await;
    // Drop into the body (Down off the last header field), go to the end, and
    // append — exercises the same edit_compose path a user would drive.
    press(&mut app, KeyCode::Down).await;
    press(&mut app, KeyCode::End).await;
    typed(&mut app, " (revised)").await;
    ctrl_d(&mut app).await;

    assert!(matches!(app.screen, Screen::MessageList));
    assert!(!app.is_editing_post(), "edit_target cleared after submit");
    let m = boards::get_message(&pool, id).await.unwrap();
    assert_eq!(m.body, "original body (revised)");
    assert!(m.edited_at.is_some(), "the edit stamps edited_at");
}

#[tokio::test]
async fn esc_cancels_an_edit_without_touching_the_post() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, pool, id) = app_on_list(pool, alice.clone(), &alice).await;

    press(&mut app, KeyCode::Char('e')).await;
    press(&mut app, KeyCode::Down).await;
    typed(&mut app, "junk").await;
    press(&mut app, KeyCode::Esc).await;

    assert!(matches!(app.screen, Screen::MessageList));
    assert!(!app.is_editing_post(), "Esc clears edit_target");
    let m = boards::get_message(&pool, id).await.unwrap();
    assert_eq!(m.body, "original body", "post untouched");
    assert!(
        m.edited_at.is_none(),
        "a cancelled edit never stamps edited_at"
    );
}

#[tokio::test]
async fn d_lets_an_author_delete_their_own_post() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, pool, id) = app_on_list(pool, alice.clone(), &alice).await;

    press(&mut app, KeyCode::Char('d')).await;

    assert!(
        boards::get_message(&pool, id).await.is_err(),
        "the post is gone"
    );
    assert!(app.messages.is_empty(), "the list refreshed");
}

#[tokio::test]
async fn a_non_author_cannot_edit_or_delete_via_the_keys() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    // Bob is viewing Alice's post.
    let (mut app, pool, id) = app_on_list(pool, bob, &alice).await;

    // `e` is a no-op for a non-author: still on the list, not the composer.
    press(&mut app, KeyCode::Char('e')).await;
    assert!(matches!(app.screen, Screen::MessageList));
    assert!(!app.is_editing_post());

    // `d` is a no-op too — Bob is not an admin and not the author.
    press(&mut app, KeyCode::Char('d')).await;
    assert!(
        boards::get_message(&pool, id).await.is_ok(),
        "Alice's post survives Bob's keypress"
    );
}
