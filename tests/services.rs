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

    let user =
        bbs_rs::services::auth::register_user(&pool, "alice", "hunter2", &Default::default())
            .await
            .unwrap();
    assert_eq!(user.role, "user");
    assert!(!user.is_guest());

    // Duplicate registration fails.
    assert!(matches!(
        bbs_rs::services::auth::register_user(&pool, "alice", "other", &Default::default()).await,
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
async fn reserved_usernames_are_rejected() {
    let pool = setup().await;
    let accounts = bbs_rs::config::Accounts::default(); // reserves root + admin

    // Default-reserved names are refused, case-insensitively and trimmed.
    for name in ["root", "admin", "ADMIN", "  Root  "] {
        assert!(
            matches!(
                bbs_rs::services::auth::register_user(&pool, name, "pw", &accounts).await,
                Err(bbs_rs::error::AppError::UsernameReserved)
            ),
            "{name:?} should be reserved"
        );
    }

    // guest is always reserved, even with an empty configured list.
    let empty = bbs_rs::config::Accounts {
        reserved_usernames: vec![],
    };
    assert!(matches!(
        bbs_rs::services::auth::register_user(&pool, "guest", "pw", &empty).await,
        Err(bbs_rs::error::AppError::UsernameReserved)
    ));

    // A non-reserved name still registers.
    assert!(
        bbs_rs::services::auth::register_user(&pool, "alice", "pw", &accounts)
            .await
            .is_ok()
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

    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw", &Default::default())
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
async fn board_acls_lock_and_moderation() {
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;

    let all = boards::list_boards(&pool).await.unwrap();
    let general = all.iter().find(|b| b.name == "General").unwrap().clone();
    let announce = all
        .iter()
        .find(|b| b.name == "Announcements")
        .unwrap()
        .clone();
    // Seed sets Announcements to admin-only writes.
    assert_eq!(announce.min_write_role, "admin");

    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let admin = auth::register_user(&pool, "adminuser", "pw", &Default::default())
        .await
        .unwrap();
    bbs_rs::services::admin::set_role(&pool, "adminuser", "admin")
        .await
        .unwrap();
    let admin = auth::find_user(&pool, &admin.username)
        .await
        .unwrap()
        .unwrap();

    // Write ACL: a regular user can't post to the admin-only board; an admin can.
    assert!(matches!(
        boards::post_message(&pool, announce.id, &alice, "s", "b").await,
        Err(AppError::BoardWriteDenied)
    ));
    boards::post_message(&pool, announce.id, &admin, "News", "hi")
        .await
        .unwrap();

    // Lock: a locked board rejects non-admins, but admins can still post.
    boards::set_locked(&pool, general.id, true).await.unwrap();
    assert!(matches!(
        boards::post_message(&pool, general.id, &alice, "s", "b").await,
        Err(AppError::BoardLocked)
    ));
    boards::post_message(&pool, general.id, &admin, "admin note", "b")
        .await
        .unwrap();
    boards::set_locked(&pool, general.id, false).await.unwrap();
    boards::post_message(&pool, general.id, &alice, "first", "b")
        .await
        .unwrap();
    boards::post_message(&pool, general.id, &alice, "second", "b")
        .await
        .unwrap();

    // Pin: a pinned message sorts to the top regardless of recency.
    let msgs = boards::list_messages(&pool, general.id).await.unwrap();
    let first = msgs.iter().find(|m| m.subject == "first").unwrap();
    assert_eq!(msgs[0].subject, "second", "newest first before pinning");
    boards::set_pinned(&pool, first.id, true).await.unwrap();
    let msgs = boards::list_messages(&pool, general.id).await.unwrap();
    assert_eq!(msgs[0].subject, "first", "pinned floats to the top");
    assert!(msgs[0].pinned);

    // Delete: moderation removes a post (leaving "second" + the admin note).
    assert!(boards::delete_message(&pool, first.id).await.unwrap());
    assert!(!boards::delete_message(&pool, first.id).await.unwrap());
    let msgs = boards::list_messages(&pool, general.id).await.unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(!msgs.iter().any(|m| m.subject == "first"));

    // Read ACL: making a board admin-read hides it from lower roles.
    boards::set_roles(&pool, "General", Some("admin"), None)
        .await
        .unwrap();
    let user_view = boards::list_readable_boards(&pool, &alice.role)
        .await
        .unwrap();
    assert!(!user_view.iter().any(|b| b.name == "General"));
    let admin_view = boards::list_readable_boards(&pool, &admin.role)
        .await
        .unwrap();
    assert!(admin_view.iter().any(|b| b.name == "General"));

    // Invalid roles are rejected.
    assert!(matches!(
        boards::set_roles(&pool, "General", Some("superuser"), None).await,
        Err(AppError::BadRole(_))
    ));
}

#[tokio::test]
async fn mail_send_read_and_guardrails() {
    let pool = setup().await;
    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = bbs_rs::services::auth::register_user(&pool, "bob", "pw", &Default::default())
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
async fn bulletins_add_list_delete() {
    let pool = setup().await;
    assert_eq!(bbs_rs::services::bulletins::count(&pool).await.unwrap(), 0);

    let id1 = bbs_rs::services::bulletins::add(&pool, "Welcome", "First bulletin")
        .await
        .unwrap();
    bbs_rs::services::bulletins::add(&pool, "Downtime", "Maintenance Sunday")
        .await
        .unwrap();

    let list = bbs_rs::services::bulletins::list(&pool).await.unwrap();
    assert_eq!(list.len(), 2);
    // Newest first.
    assert_eq!(list[0].title, "Downtime");
    assert_eq!(list[1].title, "Welcome");

    assert!(
        bbs_rs::services::bulletins::delete(&pool, id1)
            .await
            .unwrap()
    );
    assert!(
        !bbs_rs::services::bulletins::delete(&pool, id1)
            .await
            .unwrap()
    );
    assert_eq!(bbs_rs::services::bulletins::count(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn oneliners_post_list_and_guardrails() {
    use bbs_rs::services::oneliners;
    let pool = setup().await;

    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let guest = bbs_rs::services::auth::find_user(&pool, "guest")
        .await
        .unwrap()
        .unwrap();

    // Guests cannot post to the wall.
    assert!(matches!(
        oneliners::add(&pool, &guest, "hi").await,
        Err(bbs_rs::error::AppError::GuestNotAllowed)
    ));

    // Empty / whitespace-only and over-length bodies are rejected.
    assert!(matches!(
        oneliners::add(&pool, &alice, "   ").await,
        Err(bbs_rs::error::AppError::OnelinerLength(_))
    ));
    let too_long = "x".repeat(oneliners::MAX_LEN + 1);
    assert!(matches!(
        oneliners::add(&pool, &alice, &too_long).await,
        Err(bbs_rs::error::AppError::OnelinerLength(_))
    ));

    // A valid post is trimmed and stored.
    oneliners::add(&pool, &alice, "  first!  ").await.unwrap();
    oneliners::add(&pool, &alice, "second").await.unwrap();
    assert_eq!(oneliners::count(&pool).await.unwrap(), 2);

    let list = oneliners::recent(&pool, 10).await.unwrap();
    assert_eq!(list.len(), 2);
    // Newest first, with the author name joined and the body trimmed.
    assert_eq!(list[0].body, "second");
    assert_eq!(list[1].body, "first!");
    assert_eq!(list[0].author_name, "alice");

    // Moderation delete.
    assert!(oneliners::delete(&pool, list[0].id).await.unwrap());
    assert!(!oneliners::delete(&pool, list[0].id).await.unwrap());
    assert_eq!(oneliners::count(&pool).await.unwrap(), 1);
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
