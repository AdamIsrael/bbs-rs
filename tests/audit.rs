//! Moderation / audit log (#74): the in-BBS admin/board actions that should be
//! recorded, driven through the real App key handler, plus the invariant that
//! an author's own-post delete is *not* audited (only moderation is).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::{Board, User};
use bbs_rs::services::{self, audit, auth, boards, presence::Presence};
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

async fn admin(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap();
    services::admin::set_role(pool, name, "admin")
        .await
        .unwrap();
    auth::find_user(pool, name).await.unwrap().unwrap()
}

fn app_with(pool: SqlitePool, user: User) -> App {
    App::new(pool, Presence::new(), config(), user, 1, Transport::Ssh)
}

async fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        .await;
}

async fn first_board(pool: &SqlitePool) -> Board {
    boards::list_boards(pool).await.unwrap().remove(0)
}

#[tokio::test]
async fn an_admin_ban_is_audited_with_the_admin_as_actor() {
    let pool = setup().await;
    let sysop = admin(&pool, "sysop").await;
    auth::register_user(&pool, "spammer", "pw", &Default::default())
        .await
        .unwrap();

    let mut app = app_with(pool.clone(), sysop);
    app.admin_users = services::admin::list_users(&pool).await.unwrap();
    app.screen = Screen::AdminUsers;
    // Select the spammer row.
    app.admin_user_sel = app
        .admin_users
        .iter()
        .position(|u| u.username == "spammer")
        .unwrap();

    press(&mut app, KeyCode::Char('b')).await;

    let entries = audit::recent(&pool, 10).await.unwrap();
    let e = entries
        .iter()
        .find(|e| e.action == "ban_user")
        .expect("ban audited");
    assert_eq!(e.actor, "sysop");
    assert_eq!(e.target, "spammer");
}

#[tokio::test]
async fn admin_pin_lock_and_delete_are_all_audited() {
    let pool = setup().await;
    let sysop = admin(&pool, "sysop").await;
    let board = first_board(&pool).await;
    let id = boards::post_message(
        &pool,
        board.id,
        &sysop,
        "Subj",
        "body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // Pin from the message list.
    let mut app = app_with(pool.clone(), sysop);
    app.current_board = Some(board.clone());
    app.messages = boards::list_thread(&pool, board.id).await.unwrap();
    app.msg_sel = 0;
    app.screen = Screen::MessageList;
    press(&mut app, KeyCode::Char('p')).await; // pin
    press(&mut app, KeyCode::Char('d')).await; // delete (admin path)

    // Lock from the board list.
    app.boards = boards::list_readable_boards(&pool, &app.user.role)
        .await
        .unwrap();
    app.board_sel = app.boards.iter().position(|b| b.id == board.id).unwrap();
    app.screen = Screen::BoardList;
    press(&mut app, KeyCode::Char('l')).await; // lock

    let actions: Vec<String> = audit::recent(&pool, 20)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert!(actions.contains(&"pin_post".to_string()), "{actions:?}");
    assert!(actions.contains(&"delete_post".to_string()), "{actions:?}");
    assert!(actions.contains(&"lock_board".to_string()), "{actions:?}");
    let _ = id;
}

#[tokio::test]
async fn an_author_deleting_their_own_post_is_not_audited() {
    let pool = setup().await;
    // A plain (non-admin) user deleting their own post is not moderation.
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = first_board(&pool).await;
    boards::post_message(
        &pool,
        board.id,
        &alice,
        "Subj",
        "body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    let mut app = app_with(pool.clone(), alice);
    app.current_board = Some(board.clone());
    app.messages = boards::list_thread(&pool, board.id).await.unwrap();
    app.msg_sel = 0;
    app.screen = Screen::MessageList;
    press(&mut app, KeyCode::Char('d')).await; // author self-delete

    assert!(
        audit::recent(&pool, 10).await.unwrap().is_empty(),
        "self-delete must not write an audit entry"
    );
}

#[tokio::test]
async fn opening_the_audit_screen_loads_recent_entries() {
    let pool = setup().await;
    let sysop = admin(&pool, "sysop").await;
    audit::record(&pool, "sysop", "broadcast", "all sessions", Some("hi"))
        .await
        .unwrap();

    let mut app = app_with(pool.clone(), sysop);
    app.screen = Screen::AdminUsers;
    press(&mut app, KeyCode::Char('a')).await;

    assert!(matches!(app.screen, Screen::AdminAudit));
    assert_eq!(app.admin_audit.len(), 1);
    assert_eq!(app.admin_audit[0].action, "broadcast");
}
