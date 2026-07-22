//! Live chat room (#67): messages fan out through the shared `Presence` room to
//! every session currently in it, and land in each app's scrollback.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::Settings;
use bbs_rs::db::models::User;
use bbs_rs::services::{self, auth, presence::Presence};
use bbs_rs::transport::{Event, Transport};

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

fn app(pool: SqlitePool, presence: Presence, user: User, session_id: usize) -> App {
    App::new(
        pool,
        presence,
        Arc::new(Settings::default()),
        user,
        session_id,
        Transport::Ssh,
    )
}

async fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .await;
}
async fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, c).await;
    }
}

/// Drain every queued event from `rx` into `app` via the run-loop's chat path.
fn pump(app: &mut App, rx: &mut mpsc::Receiver<Event>) {
    while let Ok(ev) = rx.try_recv() {
        if let Event::Chat { from, text } = ev {
            app.push_chat_line(from, text);
        }
    }
}

#[tokio::test]
async fn a_message_reaches_everyone_in_the_room() {
    let pool = setup().await;
    let presence = Presence::new();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let carol = auth::register_user(&pool, "carol", "pw", &Default::default())
        .await
        .unwrap();

    // Three live sessions register their event senders (like the run loop does).
    let (tx1, mut rx1) = mpsc::channel(32);
    let (tx2, mut rx2) = mpsc::channel(32);
    let (tx3, mut rx3) = mpsc::channel(32);
    presence.join(1, "alice".into(), None, tx1).await;
    presence.join(2, "bob".into(), None, tx2).await;
    presence.join(3, "carol".into(), None, tx3).await;

    let mut a = app(pool.clone(), presence.clone(), alice, 1);
    let mut b = app(pool.clone(), presence.clone(), bob, 2);
    // Carol stays out of the chat room entirely.
    let _carol_app = app(pool.clone(), presence.clone(), carol, 3);

    // Alice and Bob enter the room (via the menu hotkey 'c').
    press(&mut a, 'c').await;
    assert!(matches!(a.screen, Screen::Chat));
    press(&mut b, 'c').await;

    // Alice types and sends a line.
    typed(&mut a, "hello room").await;
    a.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    assert!(a.chat_input.is_empty(), "input clears after sending");

    // Deliver queued events into each app's scrollback.
    pump(&mut a, &mut rx1);
    pump(&mut b, &mut rx2);

    // Bob sees Alice's message (and his own join notice).
    assert!(
        b.chat_log
            .iter()
            .any(|(f, t)| f == "alice" && t == "hello room"),
        "bob received the message: {:?}",
        b.chat_log
    );
    // Alice sees her own line echoed back.
    assert!(
        a.chat_log
            .iter()
            .any(|(f, t)| f == "alice" && t == "hello room"),
        "alice sees her own line"
    );
    // Carol, not in the room, received nothing.
    assert!(rx3.try_recv().is_err(), "a non-member gets no chat traffic");
}

#[tokio::test]
async fn joining_and_leaving_announce_to_the_room() {
    let pool = setup().await;
    let presence = Presence::new();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    let (tx1, mut rx1) = mpsc::channel(32);
    let (tx2, _rx2) = mpsc::channel(32);
    presence.join(1, "alice".into(), None, tx1).await;
    presence.join(2, "bob".into(), None, tx2).await;
    let mut a = app(pool.clone(), presence.clone(), alice, 1);
    let mut b = app(pool.clone(), presence.clone(), bob, 2);

    press(&mut a, 'c').await; // alice joins (only she's in the room)
    press(&mut b, 'c').await; // bob joins → alice hears it
    // Bob leaves → alice hears the departure.
    b.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await;
    assert!(matches!(b.screen, Screen::MainMenu), "Esc leaves the room");

    pump(&mut a, &mut rx1);
    let system: Vec<&String> = a
        .chat_log
        .iter()
        .filter(|(f, _)| f.is_empty())
        .map(|(_, t)| t)
        .collect();
    assert!(system.iter().any(|t| t.contains("bob joined")));
    assert!(system.iter().any(|t| t.contains("bob left")));
}

#[tokio::test]
async fn leaving_removes_the_session_from_the_room() {
    let pool = setup().await;
    let presence = Presence::new();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (tx1, _rx1) = mpsc::channel(32);
    presence.join(1, "alice".into(), None, tx1).await;
    let mut a = app(pool, presence.clone(), alice, 1);

    press(&mut a, 'c').await;
    assert_eq!(presence.chat_roster().await, vec!["alice".to_string()]);
    a.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await;
    assert!(presence.chat_roster().await.is_empty(), "left the room");
}
