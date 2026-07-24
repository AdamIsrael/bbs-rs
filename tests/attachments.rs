//! Attachments (#95): linking file-area files to posts and mail, and — the
//! part that matters — the read ACL that keeps attaching from widening access.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::{FileEntry, User};
use bbs_rs::error::AppError;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{admin, attachments, auth, boards, files, mail};
use bbs_rs::transport::Transport;

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

async fn reg(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap()
}

/// A file in a named area, creating the area at `min_read` if it's new.
async fn file_in(
    pool: &SqlitePool,
    area: &str,
    min_read: &str,
    name: &str,
    by: &User,
) -> FileEntry {
    let cfg = Settings::default().files;
    let id = match files::get_area_by_name(pool, area).await {
        Ok(a) => a.id,
        Err(_) => files::add_area(pool, area, "", Some(min_read), Some("user"))
            .await
            .unwrap(),
    };
    files::add_file(pool, id, by, name, "a test file", 10, &cfg)
        .await
        .unwrap()
}

async fn general(pool: &SqlitePool) -> i64 {
    boards::find_board_by_name(pool, "General")
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn post(pool: &SqlitePool, board_id: i64, by: &User, subject: &str) -> i64 {
    boards::post_message(
        pool,
        board_id,
        by,
        subject,
        "body",
        None,
        &Settings::default().limits,
    )
    .await
    .unwrap()
}

// ---- Service ------------------------------------------------------------

#[tokio::test]
async fn a_file_can_be_attached_to_a_post_and_read_back() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "with a file").await;

    attachments::attach_to_message(&pool, msg, f.id, &alice, &Default::default())
        .await
        .unwrap();

    let list = attachments::for_message(&pool, msg, "guest").await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].filename, "notes.txt");
    assert_eq!(list[0].path(), "Uploads/notes.txt");
}

#[tokio::test]
async fn attaching_never_widens_access() {
    let pool = setup().await;
    reg(&pool, "boss").await;
    admin::set_role(&pool, "boss", "admin").await.unwrap();
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();

    // A file in an admin-only area, attached to a post on a *public* board.
    let secret = file_in(&pool, "Staff", "admin", "payroll.txt", &boss).await;
    let public = file_in(&pool, "Uploads", "guest", "readme.txt", &boss).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &boss, "see attached").await;
    for f in [&secret, &public] {
        attachments::attach_to_message(&pool, msg, f.id, &boss, &Default::default())
            .await
            .unwrap();
    }

    // The admin sees both...
    let seen_by_admin = attachments::for_message(&pool, msg, "admin").await.unwrap();
    assert_eq!(seen_by_admin.len(), 2);

    // ...a guest sees only the public one. Not "hidden" — absent, so the
    // restricted file's very name doesn't leak.
    let seen_by_guest = attachments::for_message(&pool, msg, "guest").await.unwrap();
    assert_eq!(seen_by_guest.len(), 1);
    assert_eq!(seen_by_guest[0].filename, "readme.txt");
    assert!(!seen_by_guest.iter().any(|a| a.filename == "payroll.txt"));
}

#[tokio::test]
async fn you_cannot_attach_a_file_you_cannot_read() {
    let pool = setup().await;
    reg(&pool, "boss").await;
    admin::set_role(&pool, "boss", "admin").await.unwrap();
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();
    let alice = reg(&pool, "alice").await;

    let secret = file_in(&pool, "Staff", "admin", "payroll.txt", &boss).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "nice try").await;

    assert!(matches!(
        attachments::attach_to_message(&pool, msg, secret.id, &alice, &Default::default()).await,
        Err(AppError::NotFound)
    ));
    assert!(
        attachments::for_message(&pool, msg, "admin")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn you_cannot_attach_to_someone_elses_post() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let bob = reg(&pool, "bob").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &bob).await;
    let board = general(&pool).await;
    let alices = post(&pool, board, &alice, "mine").await;

    assert!(
        matches!(
            attachments::attach_to_message(&pool, alices, f.id, &bob, &Default::default()).await,
            Err(AppError::NotFound)
        ),
        "ownership is checked inside the INSERT, not before it"
    );
    // A post that doesn't exist answers identically, so this can't probe.
    assert!(matches!(
        attachments::attach_to_message(&pool, 9999, f.id, &bob, &Default::default()).await,
        Err(AppError::NotFound)
    ));
}

#[tokio::test]
async fn the_cap_is_enforced_and_zero_disables_it() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "many").await;
    let mut limits = Settings::default().limits;
    limits.max_attachments = 2;

    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(
            file_in(&pool, "Uploads", "guest", &format!("f{n}.txt"), &alice)
                .await
                .id,
        );
    }
    attachments::attach_to_message(&pool, msg, ids[0], &alice, &limits)
        .await
        .unwrap();
    attachments::attach_to_message(&pool, msg, ids[1], &alice, &limits)
        .await
        .unwrap();
    assert!(matches!(
        attachments::attach_to_message(&pool, msg, ids[2], &alice, &limits).await,
        Err(AppError::TooManyAttachments(2))
    ));

    limits.max_attachments = 0;
    attachments::attach_to_message(&pool, msg, ids[2], &alice, &limits)
        .await
        .unwrap();
    assert_eq!(
        attachments::for_message(&pool, msg, "guest")
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn attaching_twice_is_idempotent() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "dup").await;

    for _ in 0..2 {
        attachments::attach_to_message(&pool, msg, f.id, &alice, &Default::default())
            .await
            .unwrap();
    }
    assert_eq!(
        attachments::for_message(&pool, msg, "guest")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn deleting_the_file_or_the_post_drops_the_link() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "temp").await;
    attachments::attach_to_message(&pool, msg, f.id, &alice, &Default::default())
        .await
        .unwrap();

    files::delete_file(&pool, f.id).await.unwrap();
    assert!(
        attachments::for_message(&pool, msg, "guest")
            .await
            .unwrap()
            .is_empty(),
        "a deleted file leaves no dangling attachment"
    );

    // And deleting the post clears its rows (checked directly — the read path
    // would return empty either way).
    let f2 = file_in(&pool, "Uploads", "guest", "other.txt", &alice).await;
    attachments::attach_to_message(&pool, msg, f2.id, &alice, &Default::default())
        .await
        .unwrap();
    boards::delete_message(&pool, msg).await.unwrap();
    let left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?")
            .bind(msg)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn mail_attachments_work_the_same_way() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    reg(&pool, "bob").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;

    let id = mail::send_mail(
        &pool,
        &alice,
        "bob",
        "here you go",
        "see attached",
        &Settings::default().limits,
    )
    .await
    .unwrap();
    attachments::attach_to_mail(&pool, id, f.id, &alice, &Default::default())
        .await
        .unwrap();

    assert_eq!(
        attachments::for_mail(&pool, id, "user")
            .await
            .unwrap()
            .len(),
        1
    );
    // The recipient can't retroactively attach to mail they received.
    let bob = auth::find_user(&pool, "bob").await.unwrap().unwrap();
    assert!(matches!(
        attachments::attach_to_mail(&pool, id, f.id, &bob, &Default::default()).await,
        Err(AppError::NotFound)
    ));
}

#[tokio::test]
async fn the_picker_lists_only_readable_files() {
    let pool = setup().await;
    reg(&pool, "boss").await;
    admin::set_role(&pool, "boss", "admin").await.unwrap();
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();
    file_in(&pool, "Staff", "admin", "payroll.txt", &boss).await;
    file_in(&pool, "Uploads", "guest", "readme.txt", &boss).await;

    let as_user = attachments::pickable(&pool, "user").await.unwrap();
    assert_eq!(as_user.len(), 1);
    assert_eq!(as_user[0].filename, "readme.txt");

    let as_admin = attachments::pickable(&pool, "admin").await.unwrap();
    assert_eq!(as_admin.len(), 2);
}

// ---- TUI ----------------------------------------------------------------

fn app(pool: SqlitePool, user: User) -> App {
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

async fn ctrl(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
        .await;
}

async fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c)).await;
    }
}

/// Walk the real menu into the first board's message list.
async fn open_first_board(app: &mut App) {
    press(app, KeyCode::Char('b')).await; // Message Boards
    assert_eq!(app.screen, Screen::BoardList);
    press(app, KeyCode::Enter).await; // first board = General
    assert_eq!(app.screen, Screen::MessageList);
}

#[tokio::test]
async fn compose_a_post_with_an_attachment_end_to_end() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let mut app = app(pool.clone(), alice);

    open_first_board(&mut app).await;
    press(&mut app, KeyCode::Char('n')).await;
    assert_eq!(app.screen, Screen::ComposePost);

    // ^A opens the picker; the draft is untouched behind it.
    typed(&mut app, "Subject here").await;
    ctrl(&mut app, 'a').await;
    assert_eq!(app.screen, Screen::AttachPicker);
    assert_eq!(app.attach_pick.len(), 1);

    press(&mut app, KeyCode::Enter).await; // stage it
    assert_eq!(app.pending_attachments.len(), 1);
    press(&mut app, KeyCode::Enter).await; // Enter again unstages
    assert!(app.pending_attachments.is_empty());
    press(&mut app, KeyCode::Enter).await; // and back on

    press(&mut app, KeyCode::Esc).await;
    assert_eq!(app.screen, Screen::ComposePost, "back on the draft");
    assert_eq!(app.form.value(0), "Subject here", "with the subject intact");

    ctrl(&mut app, 'd').await; // send
    assert_eq!(app.screen, Screen::MessageList);
    assert!(app.status.contains("1 attachment"), "{}", app.status);
    assert!(app.pending_attachments.is_empty(), "staging is cleared");

    // The post really carries it.
    let msg: i64 = sqlx::query_scalar("SELECT id FROM messages ORDER BY id DESC LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        attachments::for_message(&pool, msg, "guest")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn the_reader_lists_attachments_and_opens_one() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let f = file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &alice, "with a file").await;
    attachments::attach_to_message(&pool, msg, f.id, &alice, &Default::default())
        .await
        .unwrap();

    let mut app = app(pool.clone(), alice);
    open_first_board(&mut app).await;
    press(&mut app, KeyCode::Enter).await; // read the post
    assert_eq!(app.screen, Screen::ReadMessage);
    assert_eq!(
        app.current_attachments.len(),
        1,
        "loaded with the post, so the view can name it"
    );

    press(&mut app, KeyCode::Char('a')).await;
    assert_eq!(app.screen, Screen::Attachments);
    press(&mut app, KeyCode::Esc).await;
    assert_eq!(app.screen, Screen::ReadMessage, "Esc returns to the post");
}

#[tokio::test]
async fn a_guest_sees_no_restricted_attachment_in_the_reader() {
    let pool = setup().await;
    reg(&pool, "boss").await;
    admin::set_role(&pool, "boss", "admin").await.unwrap();
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();
    let secret = file_in(&pool, "Staff", "admin", "payroll.txt", &boss).await;
    let board = general(&pool).await;
    let msg = post(&pool, board, &boss, "see attached").await;
    attachments::attach_to_message(&pool, msg, secret.id, &boss, &Default::default())
        .await
        .unwrap();

    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let mut app = app(pool.clone(), guest);
    open_first_board(&mut app).await;
    press(&mut app, KeyCode::Enter).await;
    assert_eq!(app.screen, Screen::ReadMessage);
    assert!(
        app.current_attachments.is_empty(),
        "the restricted file isn't listed for a guest"
    );

    press(&mut app, KeyCode::Char('a')).await;
    assert_eq!(app.screen, Screen::ReadMessage, "and 'a' opens nothing");
    assert!(app.status.contains("No attachments"), "{}", app.status);
}

#[tokio::test]
async fn a_remote_dm_refuses_attachments() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    file_in(&pool, "Uploads", "guest", "notes.txt", &alice).await;
    let mut app = app(pool.clone(), alice);

    press(&mut app, KeyCode::Char('m')).await; // Private Mail
    assert_eq!(app.screen, Screen::Mailbox);
    press(&mut app, KeyCode::Char('n')).await; // compose
    assert_eq!(app.screen, Screen::ComposeMail);
    typed(&mut app, "someone@remote.social").await;
    press(&mut app, KeyCode::Enter).await; // to Subject
    typed(&mut app, "hi").await;
    ctrl(&mut app, 'a').await;
    press(&mut app, KeyCode::Enter).await; // stage a file
    press(&mut app, KeyCode::Esc).await;
    assert_eq!(app.pending_attachments.len(), 1);

    ctrl(&mut app, 'd').await;
    assert_eq!(app.screen, Screen::ComposeMail, "the draft is kept");
    assert!(
        app.status.contains("remote address"),
        "told why, rather than silently dropping the file: {}",
        app.status
    );
}
