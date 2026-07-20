//! Ignore / block list (#97), driven through the real App key handler: block a
//! user from their profile, confirm their board posts are hidden and pages are
//! refused, and unblock from the Ignored Users screen.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, blocks, boards, presence::Presence};
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

fn app_with(pool: SqlitePool, user: User) -> App {
    App::new(pool, Presence::new(), config(), user, 1, Transport::Ssh)
}

async fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        .await;
}

#[tokio::test]
async fn b_on_a_profile_blocks_and_toggles_off() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    let mut app = app_with(pool.clone(), alice.clone());
    // View bob's profile (as `open_profile` leaves it).
    app.current_profile = Some(
        bbs_rs::services::profiles::get_profile(&pool, bob.id)
            .await
            .unwrap(),
    );
    app.screen = Screen::Profile;

    press(&mut app, KeyCode::Char('b')).await;
    assert!(app.current_profile_blocked);
    assert!(blocks::is_blocked(&pool, alice.id, bob.id).await.unwrap());

    // Toggle off.
    press(&mut app, KeyCode::Char('b')).await;
    assert!(!app.current_profile_blocked);
    assert!(!blocks::is_blocked(&pool, alice.id, bob.id).await.unwrap());
}

#[tokio::test]
async fn a_blocked_authors_posts_are_hidden_from_the_reader() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    boards::post_message(
        &pool,
        board.id,
        &alice,
        "Hi",
        "from alice",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::post_message(
        &pool,
        board.id,
        &bob,
        "Yo",
        "from bob",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    blocks::block(&pool, &alice, &bob).await.unwrap();

    // alice opens the board through the real board-list Enter path — this is
    // where the filter must run (not only on a later reload).
    let mut app = app_with(pool.clone(), alice.clone());
    app.boards = boards::list_readable_boards(&pool, &alice.role)
        .await
        .unwrap();
    app.board_sel = app.boards.iter().position(|b| b.id == board.id).unwrap();
    app.screen = Screen::BoardList;
    press(&mut app, KeyCode::Enter).await;

    assert!(matches!(app.screen, Screen::MessageList));
    let subjects: Vec<String> = app
        .messages
        .iter()
        .map(|m| m.message.subject.clone())
        .collect();
    assert_eq!(
        subjects,
        vec!["Hi".to_string()],
        "bob's post hidden on open"
    );

    // And still hidden after an in-place reload.
    app.reload_messages_for_test().await;
    let after: Vec<String> = app
        .messages
        .iter()
        .map(|m| m.message.subject.clone())
        .collect();
    assert_eq!(after, vec!["Hi".to_string()], "bob's post hidden on reload");
}

#[tokio::test]
async fn a_page_from_a_blocked_user_is_refused() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    // alice blocks bob; alice is online (holds the receiver).
    blocks::block(&pool, &alice, &bob).await.unwrap();
    let presence = Presence::new();
    let (tx, mut rx) = mpsc::channel(4);
    presence.join(7, "alice".into(), None, tx).await;

    // bob tries to page alice.
    let mut app = App::new(pool.clone(), presence, config(), bob, 1, Transport::Ssh);
    app.online = vec![bbs_rs::services::presence::OnlineUser {
        username: "alice".into(),
        since: 0,
    }];
    app.who_sel = 0;
    app.screen = Screen::WhoOnline;
    press(&mut app, KeyCode::Char('p')).await; // begin page
    for c in "hey".chars() {
        press(&mut app, KeyCode::Char(c)).await;
    }
    press(&mut app, KeyCode::Enter).await;

    assert!(app.status.contains("isn't accepting pages"));
    assert!(rx.try_recv().is_err(), "no page delivered to a blocker");
}

#[tokio::test]
async fn the_ignore_list_screen_unblocks() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    blocks::block(&pool, &alice, &bob).await.unwrap();

    // From alice's own profile, `i` opens the ignore list; `u` unblocks.
    let mut app = app_with(pool.clone(), alice.clone());
    app.current_profile = Some(
        bbs_rs::services::profiles::get_profile(&pool, alice.id)
            .await
            .unwrap(),
    );
    app.screen = Screen::Profile;
    press(&mut app, KeyCode::Char('i')).await;
    assert!(matches!(app.screen, Screen::IgnoreList));
    assert_eq!(app.ignored.len(), 1);

    press(&mut app, KeyCode::Char('u')).await;
    assert!(app.ignored.is_empty());
    assert!(!blocks::is_blocked(&pool, alice.id, bob.id).await.unwrap());
}
