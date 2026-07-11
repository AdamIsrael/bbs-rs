//! Integration tests for the domain services against an in-memory SQLite DB.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    bbs_rs::services::seed(&pool).await.expect("seed");
    pool
}

#[tokio::test]
async fn guest_seeded_and_login_works() {
    let pool = setup().await;

    let guest = bbs_rs::services::auth::verify_login(&pool, "guest", "guest")
        .await
        .unwrap();
    assert!(guest.is_some(), "guest/guest should authenticate");
    assert!(guest.unwrap().is_guest());

    // Wrong password is rejected.
    assert!(
        bbs_rs::services::auth::verify_login(&pool, "guest", "nope")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn register_then_login() {
    let pool = setup().await;

    let user = bbs_rs::services::auth::register_user(&pool, "alice", "hunter2")
        .await
        .unwrap();
    assert_eq!(user.role, "user");
    assert!(!user.is_guest());

    // Duplicate registration fails.
    assert!(matches!(
        bbs_rs::services::auth::register_user(&pool, "alice", "other").await,
        Err(bbs_rs::error::AppError::UsernameTaken)
    ));

    // Registered user can log in.
    assert!(
        bbs_rs::services::auth::verify_login(&pool, "alice", "hunter2")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn guest_cannot_post_but_users_can() {
    let pool = setup().await;
    let boards = bbs_rs::services::boards::list_boards(&pool).await.unwrap();
    assert!(!boards.is_empty(), "default boards should be seeded");
    let board_id = boards[0].id;

    let guest = bbs_rs::services::auth::find_user(&pool, "guest")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        bbs_rs::services::boards::post_message(&pool, board_id, &guest, "hi", "body").await,
        Err(bbs_rs::error::AppError::GuestNotAllowed)
    ));

    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw")
        .await
        .unwrap();
    bbs_rs::services::boards::post_message(&pool, board_id, &alice, "Hello", "world")
        .await
        .unwrap();

    let messages = bbs_rs::services::boards::list_messages(&pool, board_id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].subject, "Hello");
    assert_eq!(messages[0].author_name, "alice");
}

#[tokio::test]
async fn mail_send_read_and_guardrails() {
    let pool = setup().await;
    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw")
        .await
        .unwrap();
    let bob = bbs_rs::services::auth::register_user(&pool, "bob", "pw")
        .await
        .unwrap();

    // Unknown recipient rejected.
    assert!(matches!(
        bbs_rs::services::mail::send_mail(&pool, &alice, "nobody", "s", "b").await,
        Err(bbs_rs::error::AppError::RecipientNotFound)
    ));

    bbs_rs::services::mail::send_mail(&pool, &alice, "bob", "Hi Bob", "hello")
        .await
        .unwrap();

    let inbox = bbs_rs::services::mail::inbox(&pool, bob.id).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from_name, "alice");
    assert!(inbox[0].read_at.is_none(), "new mail is unread");

    let read = bbs_rs::services::mail::read_mail(&pool, inbox[0].id, bob.id)
        .await
        .unwrap();
    assert!(read.read_at.is_some(), "reading marks it read");
}

#[tokio::test]
async fn presence_join_and_leave() {
    let presence = bbs_rs::services::presence::Presence::new();
    let (tx1, _rx1) = tokio::sync::mpsc::channel(1);
    let (tx2, _rx2) = tokio::sync::mpsc::channel(1);
    presence.join(1, "alice".into(), None, tx1).await;
    presence
        .join(2, "bob".into(), Some("10.0.0.2".into()), tx2)
        .await;
    assert_eq!(presence.list().await.len(), 2);
    presence.leave(1).await;
    let online = presence.list().await;
    assert_eq!(online.len(), 1);
    assert_eq!(online[0].username, "bob");
}

#[tokio::test]
async fn presence_kick_signals_matching_sessions() {
    use bbs_rs::transport::Event;
    use std::collections::HashSet;

    let presence = bbs_rs::services::presence::Presence::new();
    let (tx_user, mut rx_user) = tokio::sync::mpsc::channel(1);
    let (tx_ip, mut rx_ip) = tokio::sync::mpsc::channel(1);
    let (tx_safe, mut rx_safe) = tokio::sync::mpsc::channel(1);
    presence
        .join(1, "alice".into(), Some("1.1.1.1".into()), tx_user)
        .await;
    presence
        .join(2, "bob".into(), Some("2.2.2.2".into()), tx_ip)
        .await;
    presence
        .join(3, "carol".into(), Some("3.3.3.3".into()), tx_safe)
        .await;

    let banned_users = HashSet::from(["alice".to_string()]);
    let banned_ips = HashSet::from(["2.2.2.2".to_string()]);
    let kicked = presence.kick(&banned_users, &banned_ips).await;
    assert_eq!(kicked, 2, "alice (by name) and bob (by ip) are kicked");

    assert!(matches!(rx_user.try_recv(), Ok(Event::Quit)));
    assert!(matches!(rx_ip.try_recv(), Ok(Event::Quit)));
    assert!(rx_safe.try_recv().is_err(), "carol is not signalled");
}
