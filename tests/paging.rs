//! User paging ("yell"), driven through the real App key handler (#68): from
//! the Who's Online screen, `p` opens a one-line composer and Enter fans the
//! page out to the target's live sessions via the presence registry.

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

/// alice's app, on the Who's Online screen, with `target` shown online (and, if
/// `target_online`, actually registered in presence so a page can land). Returns
/// the app and a receiver for the target's session events.
async fn app_on_who(
    pool: SqlitePool,
    alice: User,
    target: &str,
    target_online: bool,
) -> (App, Presence, mpsc::Receiver<Event>) {
    let presence = Presence::new();
    let (tx, rx) = mpsc::channel(4);
    if target_online {
        presence.join(99, target.to_string(), None, tx).await;
    }
    let mut app = App::new(pool, presence.clone(), config(), alice, 1, Transport::Ssh);
    // Put the app where `open_who` leaves it: roster loaded, on the Who screen.
    app.online = presence.list().await;
    // The roster may be empty if the target isn't online; make sure the target
    // is selectable regardless, mirroring what the user sees after a page target
    // logs in. When target_online is false we inject a stale roster entry.
    if !target_online {
        app.online = vec![bbs_rs::services::presence::OnlineUser {
            username: target.to_string(),
            since: 0,
        }];
    }
    app.who_sel = 0;
    app.screen = Screen::WhoOnline;
    (app, presence, rx)
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
async fn p_opens_the_page_composer_for_the_selected_user() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _presence, _rx) = app_on_who(pool, alice, "bob", true).await;

    press(&mut app, KeyCode::Char('p')).await;

    assert!(matches!(app.screen, Screen::ComposePage));
    assert_eq!(app.page_target(), Some("bob"), "remembers who we're paging");
}

#[tokio::test]
async fn sending_a_page_delivers_it_and_returns_to_who() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _presence, mut rx) = app_on_who(pool, alice, "bob", true).await;

    press(&mut app, KeyCode::Char('p')).await;
    typed(&mut app, "coffee?").await;
    press(&mut app, KeyCode::Enter).await;

    assert!(
        matches!(app.screen, Screen::WhoOnline),
        "back to the roster"
    );
    assert_eq!(app.page_target(), None, "target cleared after send");
    assert_eq!(app.status, "Paged bob.");
    match rx.try_recv() {
        Ok(Event::Paged { from, body }) => {
            assert_eq!(from, "alice");
            assert_eq!(body, "coffee?");
        }
        other => panic!("bob should have received the page, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_page_is_refused_and_keeps_composing() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _presence, mut rx) = app_on_who(pool, alice, "bob", true).await;

    press(&mut app, KeyCode::Char('p')).await;
    press(&mut app, KeyCode::Enter).await; // nothing typed

    assert!(matches!(app.screen, Screen::ComposePage), "still composing");
    assert_eq!(app.page_target(), Some("bob"), "target retained");
    assert!(rx.try_recv().is_err(), "no empty page delivered");
}

#[tokio::test]
async fn paging_a_user_who_left_reports_it() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    // Roster shows bob (stale), but he isn't actually in presence.
    let (mut app, _presence, _rx) = app_on_who(pool, alice, "bob", false).await;

    press(&mut app, KeyCode::Char('p')).await;
    typed(&mut app, "you there?").await;
    press(&mut app, KeyCode::Enter).await;

    assert!(matches!(app.screen, Screen::WhoOnline));
    assert_eq!(app.status, "bob is no longer online.");
}

#[tokio::test]
async fn esc_cancels_a_page() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let (mut app, _presence, mut rx) = app_on_who(pool, alice, "bob", true).await;

    press(&mut app, KeyCode::Char('p')).await;
    typed(&mut app, "nvm").await;
    press(&mut app, KeyCode::Esc).await;

    assert!(matches!(app.screen, Screen::WhoOnline));
    assert_eq!(app.page_target(), None);
    assert!(rx.try_recv().is_err(), "a cancelled page is never sent");
}

#[tokio::test]
async fn a_guest_cannot_page() {
    let pool = setup().await;
    // The seeded guest account is a guest role.
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let (mut app, _presence, mut rx) = app_on_who(pool, guest, "bob", true).await;

    press(&mut app, KeyCode::Char('p')).await;

    assert!(
        matches!(app.screen, Screen::WhoOnline),
        "no composer for guests"
    );
    assert!(app.status.contains("Guests cannot page"));
    assert!(rx.try_recv().is_err());
}
