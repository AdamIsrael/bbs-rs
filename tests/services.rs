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
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .expect("seed");
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

/// Usernames are restricted to a URI-safe ASCII set. This is a security
/// boundary, not cosmetics: federated actors are stored in `users` keyed by a
/// fully-qualified `alice@remote.social` handle, so a local account containing
/// `@` could impersonate a remote one.
#[tokio::test]
async fn register_rejects_unsafe_usernames() {
    use bbs_rs::error::AppError;
    use bbs_rs::services::auth::register_user;
    let pool = setup().await;
    let accounts = Default::default();

    for bad in [
        "alice@remote.social", // impersonates a federated handle
        "a@b",
        "alice/bob", // breaks out of an actor URI path
        "alice bob", // whitespace
        " alice",    // leading space would collide with "alice" on lookup
        "alice ",
        "alice\tbob",
        "alice\nbob",
        "",
        "   ",
        "aliceßé", // non-ASCII: not safely representable in preferredUsername
        "alice:8088",
        "alice#main",
        "alice?q=1",
        &"a".repeat(33), // over MAX_USERNAME_CHARS
    ] {
        assert!(
            matches!(
                register_user(&pool, bad, "pw", &accounts).await,
                Err(AppError::UsernameInvalid(_))
            ),
            "username {bad:?} must be rejected"
        );
    }

    // The allowed set still works.
    for good in ["alice", "bob_2", "carol-x", "dave.jr", &"a".repeat(32)] {
        assert!(
            register_user(&pool, good, "pw", &accounts).await.is_ok(),
            "username {good:?} should be accepted"
        );
    }
}

#[tokio::test]
async fn message_threads_nest_and_order() {
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // Two top-level threads.
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "A",
        "root a",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "B",
        "root b",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    let roots = boards::list_thread(&pool, general.id).await.unwrap();
    let a_id = roots
        .iter()
        .find(|t| t.message.subject == "A")
        .unwrap()
        .message
        .id;

    // Two replies to A, and a reply to the first reply (depth 2).
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Re: A",
        "r1",
        Some(a_id),
        &Default::default(),
    )
    .await
    .unwrap();
    let after = boards::list_thread(&pool, general.id).await.unwrap();
    let r1_id = after
        .iter()
        .find(|t| t.message.body == "r1")
        .unwrap()
        .message
        .id;
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Re: A",
        "r2",
        Some(a_id),
        &Default::default(),
    )
    .await
    .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Re: Re: A",
        "r1a",
        Some(r1_id),
        &Default::default(),
    )
    .await
    .unwrap();

    // Depth-first order: newest root (B) first, then A and its replies (oldest
    // first, with the nested reply under r1).
    let thread = boards::list_thread(&pool, general.id).await.unwrap();
    let shape: Vec<(u16, &str)> = thread
        .iter()
        .map(|t| (t.depth, t.message.body.as_str()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (0, "root b"),
            (0, "root a"),
            (1, "r1"),
            (2, "r1a"),
            (1, "r2"),
        ]
    );

    // Replying to a nonexistent parent is rejected.
    assert!(matches!(
        boards::post_message(
            &pool,
            general.id,
            &alice,
            "x",
            "y",
            Some(999_999),
            &Default::default()
        )
        .await,
        Err(AppError::NotFound)
    ));
}

#[tokio::test]
async fn unread_counts_and_watermark() {
    use bbs_rs::services::{auth, boards};
    use bbs_rs::util::now_unix;
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    // No messages yet: nothing unread, no watermark recorded.
    assert!(
        boards::unread_counts(&pool, alice.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        boards::last_seen(&pool, alice.id, general.id)
            .await
            .unwrap(),
        0
    );

    // Bob posts two; both are unread for Alice but not for Bob (own posts).
    for body in ["one", "two"] {
        boards::post_message(
            &pool,
            general.id,
            &bob,
            "s",
            body,
            None,
            &Default::default(),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        boards::unread_counts(&pool, alice.id)
            .await
            .unwrap()
            .get(&general.id),
        Some(&2)
    );
    assert!(
        boards::unread_counts(&pool, bob.id)
            .await
            .unwrap()
            .is_empty()
    );

    // Alice replies; now Bob has one unread, Alice still has Bob's two.
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "s",
        "re",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        boards::unread_counts(&pool, bob.id)
            .await
            .unwrap()
            .get(&general.id),
        Some(&1)
    );
    assert_eq!(
        boards::unread_counts(&pool, alice.id)
            .await
            .unwrap()
            .get(&general.id),
        Some(&2)
    );

    // Marking the board seen (watermark past every post) clears Alice's unread.
    let future = now_unix() + 3600;
    boards::mark_board_seen(&pool, alice.id, general.id, future)
        .await
        .unwrap();
    assert!(
        boards::unread_counts(&pool, alice.id)
            .await
            .unwrap()
            .is_empty()
    );

    // The watermark only moves forward: an earlier mark can't reveal old posts.
    boards::mark_board_seen(&pool, alice.id, general.id, 1)
        .await
        .unwrap();
    assert_eq!(
        boards::last_seen(&pool, alice.id, general.id)
            .await
            .unwrap(),
        future
    );
    assert!(
        boards::unread_counts(&pool, alice.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn profiles_update_and_stats() {
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, boards, profiles};
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // A fresh profile is blank with zero posts and no recorded login.
    let p = profiles::get_profile(&pool, alice.id).await.unwrap();
    assert_eq!(p.username, "alice");
    assert_eq!(p.real_name, "");
    assert_eq!(p.post_count, 0);
    assert_eq!(p.last_login, None);

    // Update fields (trimmed) and read them back, including by username.
    profiles::update_profile(&pool, alice.id, "  Alice A  ", "Toronto", "hi", "cheers")
        .await
        .unwrap();
    let p = profiles::get_profile_by_name(&pool, "alice").await.unwrap();
    assert_eq!(p.real_name, "Alice A");
    assert_eq!(p.location, "Toronto");
    assert_eq!(
        profiles::signature_of(&pool, alice.id).await.unwrap(),
        "cheers"
    );

    // Posting bumps the counted post total.
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "s",
        "b",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        profiles::get_profile(&pool, alice.id)
            .await
            .unwrap()
            .post_count,
        1
    );

    // An over-long field is rejected (nothing is silently truncated).
    let long = "x".repeat(profiles::MAX_TAGLINE + 1);
    assert!(matches!(
        profiles::update_profile(&pool, alice.id, "", "", &long, "").await,
        Err(AppError::FieldTooLong("Tagline", _))
    ));

    // Unknown user id has no profile.
    assert!(matches!(
        profiles::get_profile(&pool, 999_999).await,
        Err(AppError::NotFound)
    ));
}

#[tokio::test]
async fn stats_totals_leaderboard_and_callers() {
    use bbs_rs::services::{admin, auth, boards, stats};
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    // Alice posts twice, Bob once → Alice leads the board.
    for body in ["a1", "a2"] {
        boards::post_message(
            &pool,
            general.id,
            &alice,
            "s",
            body,
            None,
            &Default::default(),
        )
        .await
        .unwrap();
    }
    boards::post_message(
        &pool,
        general.id,
        &bob,
        "s",
        "b1",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // Two successful calls (bob most recent) and one failure (not counted).
    admin::record_login(&pool, "alice", None, true)
        .await
        .unwrap();
    admin::record_login(&pool, "bob", None, true).await.unwrap();
    admin::record_login(&pool, "mallory", None, false)
        .await
        .unwrap();

    let s = stats::gather(&pool, stats::LIST_LIMIT).await.unwrap();
    // guest is seeded alongside alice + bob.
    assert_eq!(s.total_users, 3);
    assert_eq!(s.total_posts, 3);
    assert_eq!(s.total_calls, 2, "failed logins are excluded");

    // Leaderboard: alice (2) ahead of bob (1); guest never posted.
    assert_eq!(s.top_posters.len(), 2);
    assert_eq!(s.top_posters[0].username, "alice");
    assert_eq!(s.top_posters[0].posts, 2);
    assert_eq!(s.top_posters[1].username, "bob");

    // Recent callers: one row per successful user (the failed 'mallory' is
    // excluded). Order between same-second calls isn't asserted.
    let mut callers: Vec<&str> = s
        .recent_callers
        .iter()
        .map(|c| c.username.as_str())
        .collect();
    callers.sort();
    assert_eq!(callers, vec!["alice", "bob"]);
}

#[tokio::test]
async fn search_matches_ranks_and_respects_acl() {
    use bbs_rs::services::{auth, boards, search};
    let pool = setup().await;
    let boards_list = boards::list_boards(&pool).await.unwrap();
    let general = boards_list.iter().find(|b| b.name == "General").unwrap();
    // Announcements is admin-only to post; make it admin-only to *read* too so
    // we can prove the ACL filter hides it from a regular user.
    let announce = boards_list
        .iter()
        .find(|b| b.name == "Announcements")
        .unwrap();
    boards::set_roles(&pool, "Announcements", Some("admin"), None)
        .await
        .unwrap();

    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let admin = auth::register_user(&pool, "adminuser", "pw", &Default::default())
        .await
        .unwrap();
    bbs_rs::services::admin::set_role(&pool, "adminuser", "admin")
        .await
        .unwrap();
    let admin = bbs_rs::services::auth::find_user(&pool, &admin.username)
        .await
        .unwrap()
        .unwrap();

    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Rust tips",
        "pattern matching is great",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Cooking",
        "how to bake bread",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    // A secret post in the admin-only board that also mentions "bread".
    boards::post_message(
        &pool,
        announce.id,
        &admin,
        "Secret recipe",
        "the secret bread formula",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // Body term matches; punctuation/case-insensitive.
    let hits = search::search_messages(&pool, "user", "MATCHING", 50)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject, "Rust tips");

    // "bread" appears in General and the admin-only Announcements board.
    let as_user = search::search_messages(&pool, "user", "bread", 50)
        .await
        .unwrap();
    assert_eq!(
        as_user.len(),
        1,
        "regular user can't see the admin board hit"
    );
    assert_eq!(as_user[0].subject, "Cooking");
    let as_admin = search::search_messages(&pool, "admin", "bread", 50)
        .await
        .unwrap();
    assert_eq!(as_admin.len(), 2, "admin sees both boards");

    // Blank query returns nothing; a non-matching term too.
    assert!(
        search::search_messages(&pool, "admin", "   ", 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        search::search_messages(&pool, "admin", "zzznope", 50)
            .await
            .unwrap()
            .is_empty()
    );

    // Deleting a message drops it from the index (trigger keeps FTS in sync).
    let cooking = boards::list_messages(&pool, general.id)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.subject == "Cooking")
        .unwrap();
    assert!(boards::delete_message(&pool, cooking.id).await.unwrap());
    assert!(
        search::search_messages(&pool, "user", "bread", 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn subject_and_body_length_limits() {
    use bbs_rs::config::Limits;
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, boards, mail};
    let pool = setup().await;
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    // Tight caps; rate limiting disabled so it doesn't interfere.
    let limits = Limits {
        window_secs: 0,
        max_posts: 0,
        max_mail: 0,
        max_oneliners: 0,
        max_subject_chars: 5,
        max_body_chars: 10,
    };

    // Over-long subject / body are rejected; within-limit posts succeed.
    assert!(matches!(
        boards::post_message(&pool, general.id, &alice, "toolong", "ok", None, &limits).await,
        Err(AppError::FieldTooLong("Subject", 5))
    ));
    assert!(matches!(
        boards::post_message(
            &pool,
            general.id,
            &alice,
            "s",
            "this body is way too long",
            None,
            &limits
        )
        .await,
        Err(AppError::FieldTooLong("Message", 10))
    ));
    boards::post_message(&pool, general.id, &alice, "hi", "short", None, &limits)
        .await
        .unwrap();

    // Mail honors the same caps.
    assert!(matches!(
        mail::send_mail(&pool, &alice, "bob", "toolong", "ok", &limits).await,
        Err(AppError::FieldTooLong("Subject", 5))
    ));
    mail::send_mail(&pool, &alice, "bob", "hi", "short", &limits)
        .await
        .unwrap();

    // A 0 cap disables the limit — a long subject/body is accepted.
    let no_cap = Limits {
        max_subject_chars: 0,
        max_body_chars: 0,
        ..limits
    };
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "a very long subject indeed",
        "and a considerably longer body than before",
        None,
        &no_cap,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn seed_uses_configured_boards_and_guest_password() {
    use bbs_rs::config::{Seed, SeedBoard};
    use bbs_rs::services::{self, auth, boards};

    // A bare migrated DB (no default seeding).
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let seed = Seed {
        guest_password: Some("swordfish".into()),
        boards: Some(vec![
            SeedBoard {
                name: "Lobby".into(),
                description: "hi".into(),
                min_read: "guest".into(),
                min_write: "user".into(),
            },
            SeedBoard {
                name: "Staff".into(),
                description: String::new(),
                min_read: "admin".into(),
                min_write: "admin".into(),
            },
        ]),
    };
    services::seed(&pool, &seed).await.unwrap();

    // Exactly the configured boards exist (not the built-in General/Announcements).
    let all = boards::list_boards(&pool).await.unwrap();
    let names: Vec<&str> = all.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["Lobby", "Staff"]);
    let staff = all.iter().find(|b| b.name == "Staff").unwrap();
    assert_eq!(staff.min_read_role, "admin");
    assert_eq!(staff.min_write_role, "admin");

    // Guest authenticates with the configured password, not the default.
    assert!(
        auth::verify_login(&pool, "guest", "swordfish")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        auth::verify_login(&pool, "guest", "guest")
            .await
            .unwrap()
            .is_none()
    );

    // Re-seeding is a no-op once boards exist.
    services::seed(&pool, &Default::default()).await.unwrap();
    assert_eq!(boards::list_boards(&pool).await.unwrap().len(), 2);
}

#[tokio::test]
async fn db_backup_into_produces_a_readable_copy() {
    use bbs_rs::db;
    use bbs_rs::services::{self, auth};

    // A file-backed source DB (VACUUM INTO snapshots an on-disk database, as in
    // production).
    let dir = std::env::temp_dir().join(format!("bbs_bk_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.db");
    let _ = std::fs::remove_file(&src);
    let pool = db::connect(&format!("sqlite://{}?mode=rwc", src.display()))
        .await
        .unwrap();
    db::run_migrations(&pool).await.unwrap();
    services::seed(&pool, &Default::default()).await.unwrap();
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    let dest = dir.join("snap.db");
    let _ = std::fs::remove_file(&dest);
    db::backup_into(&pool, &dest).await.unwrap();
    assert!(dest.exists(), "backup file should be written");

    // The snapshot opens as a valid database with the same data.
    let bk = db::connect(&format!("sqlite://{}?mode=ro", dest.display()))
        .await
        .unwrap();
    assert!(auth::find_user(&bk, "alice").await.unwrap().is_some());
    assert!(auth::find_user(&bk, "guest").await.unwrap().is_some());

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dest);
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
        bbs_rs::services::boards::post_message(
            &pool,
            board_id,
            &guest,
            "hi",
            "body",
            None,
            &Default::default()
        )
        .await,
        Err(bbs_rs::error::AppError::GuestNotAllowed)
    ));

    let alice = bbs_rs::services::auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    bbs_rs::services::boards::post_message(
        &pool,
        board_id,
        &alice,
        "Hello",
        "world",
        None,
        &Default::default(),
    )
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
        boards::post_message(
            &pool,
            announce.id,
            &alice,
            "s",
            "b",
            None,
            &Default::default()
        )
        .await,
        Err(AppError::BoardWriteDenied)
    ));
    boards::post_message(
        &pool,
        announce.id,
        &admin,
        "News",
        "hi",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    // Lock: a locked board rejects non-admins, but admins can still post.
    boards::set_locked(&pool, general.id, true).await.unwrap();
    assert!(matches!(
        boards::post_message(
            &pool,
            general.id,
            &alice,
            "s",
            "b",
            None,
            &Default::default()
        )
        .await,
        Err(AppError::BoardLocked)
    ));
    boards::post_message(
        &pool,
        general.id,
        &admin,
        "admin note",
        "b",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::set_locked(&pool, general.id, false).await.unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "first",
        "b",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "second",
        "b",
        None,
        &Default::default(),
    )
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
async fn rate_limits_throttle_non_admins() {
    use bbs_rs::config::Limits;
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, boards, mail, oneliners};
    let pool = setup().await;

    let limits = Limits {
        window_secs: 60,
        max_posts: 2,
        max_mail: 2,
        max_oneliners: 2,
        ..Default::default()
    };
    let general = boards::list_boards(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.name == "General")
        .unwrap();

    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::find_user(&pool, "bob").await.unwrap().unwrap();

    // Posts: two allowed within the window, the third is throttled.
    boards::post_message(&pool, general.id, &alice, "1", "b", None, &limits)
        .await
        .unwrap();
    boards::post_message(&pool, general.id, &alice, "2", "b", None, &limits)
        .await
        .unwrap();
    assert!(matches!(
        boards::post_message(&pool, general.id, &alice, "3", "b", None, &limits).await,
        Err(AppError::RateLimited)
    ));

    // Mail and oneliners throttle independently, per user.
    mail::send_mail(&pool, &alice, "bob", "s", "b", &limits)
        .await
        .unwrap();
    mail::send_mail(&pool, &alice, "bob", "s", "b", &limits)
        .await
        .unwrap();
    assert!(matches!(
        mail::send_mail(&pool, &alice, "bob", "s", "b", &limits).await,
        Err(AppError::RateLimited)
    ));
    oneliners::add(&pool, &alice, "a", &limits, &Default::default())
        .await
        .unwrap();
    oneliners::add(&pool, &alice, "b", &limits, &Default::default())
        .await
        .unwrap();
    assert!(matches!(
        oneliners::add(&pool, &alice, "c", &limits, &Default::default()).await,
        Err(AppError::RateLimited)
    ));

    // Admins are never throttled.
    auth::register_user(&pool, "adminuser", "pw", &Default::default())
        .await
        .unwrap();
    bbs_rs::services::admin::set_role(&pool, "adminuser", "admin")
        .await
        .unwrap();
    let admin = auth::find_user(&pool, "adminuser").await.unwrap().unwrap();
    for i in 0..5 {
        boards::post_message(
            &pool,
            general.id,
            &admin,
            &format!("m{i}"),
            "b",
            None,
            &limits,
        )
        .await
        .unwrap();
    }

    // A zero cap disables that limit; a zero window disables all of them.
    let no_cap = Limits {
        window_secs: 60,
        max_posts: 0,
        max_mail: 0,
        max_oneliners: 0,
        ..Default::default()
    };
    for i in 0..5 {
        boards::post_message(
            &pool,
            general.id,
            &bob,
            &format!("x{i}"),
            "b",
            None,
            &no_cap,
        )
        .await
        .unwrap();
    }
    let no_window = Limits {
        window_secs: 0,
        max_posts: 2,
        max_mail: 2,
        max_oneliners: 2,
        ..Default::default()
    };
    for i in 0..5 {
        oneliners::add(
            &pool,
            &bob,
            &format!("y{i}"),
            &no_window,
            &Default::default(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn pubkey_register_authorize_and_delete() {
    use bbs_rs::error::AppError;
    use bbs_rs::services::{admin, auth, keys};
    use bbs_rs::ssh::pubkey;
    const K1: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJUWRla9Q/lGz4Xu2VckCtOLy1oQQIaFUAfak+oJMNO9 alice@laptop";
    const K2: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILIDJ3mlwBDZiIC4VrfpTcrFBGOlAKltRIoPjd2ONkD2 bob@desktop";
    let pool = setup().await;

    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // Parsing extracts algorithm / fingerprint / comment.
    let parsed = pubkey::parse(K1).unwrap();
    assert_eq!(parsed.algorithm, "ssh-ed25519");
    assert!(parsed.fingerprint.starts_with("SHA256:"));
    assert_eq!(parsed.comment, "alice@laptop");

    // Register it for alice; the label defaults to the key's comment.
    pubkey::register(&pool, alice.id, K1, "").await.unwrap();
    let list = keys::list_keys(&pool, alice.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label, "alice@laptop");
    let fpr = list[0].fingerprint.clone();
    let key_id = list[0].id;

    // Duplicate and invalid registrations are rejected.
    assert!(matches!(
        pubkey::register(&pool, alice.id, K1, "").await,
        Err(AppError::KeyExists)
    ));
    assert!(matches!(
        pubkey::register(&pool, alice.id, "not a key", "").await,
        Err(AppError::InvalidKey(_))
    ));

    // Authorization matches the right (username, fingerprint) pair only.
    assert!(keys::is_authorized(&pool, "alice", &fpr).await.unwrap());
    assert_eq!(
        keys::find_authorized(&pool, "alice", &fpr)
            .await
            .unwrap()
            .unwrap()
            .id,
        alice.id
    );
    assert!(!keys::is_authorized(&pool, "guest", &fpr).await.unwrap());
    let other = pubkey::parse(K2).unwrap();
    assert!(
        !keys::is_authorized(&pool, "alice", &other.fingerprint)
            .await
            .unwrap()
    );

    // The full login helper authenticates a known key and rejects an unknown one.
    assert_eq!(
        auth::attempt_pubkey_login(&pool, "alice", &fpr, Some("1.2.3.4"))
            .await
            .unwrap()
            .unwrap()
            .username,
        "alice"
    );
    assert!(
        auth::attempt_pubkey_login(&pool, "alice", &other.fingerprint, None)
            .await
            .unwrap()
            .is_none()
    );

    // A banned account can't authenticate even with a valid key.
    admin::ban_user(&pool, "alice").await.unwrap();
    assert!(
        auth::attempt_pubkey_login(&pool, "alice", &fpr, None)
            .await
            .unwrap()
            .is_none()
    );
    admin::unban_user(&pool, "alice").await.unwrap();

    // Deletion is scoped to the owner.
    assert!(!keys::delete_key(&pool, 999, key_id).await.unwrap());
    assert!(keys::delete_key(&pool, alice.id, key_id).await.unwrap());
    assert!(keys::list_keys(&pool, alice.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn file_areas_upload_accounting_and_acl() {
    use bbs_rs::config::Files;
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, files};
    use std::path::PathBuf;

    let pool = setup().await;
    // seed() creates a default "Uploads" area.
    let uploads = files::get_area_by_name(&pool, "Uploads").await.unwrap();

    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // Small limits: 100-byte files, 150-byte quota, only .txt allowed.
    let cfg = Files {
        storage_dir: PathBuf::from("unused-in-tests"),
        max_file_bytes: 100,
        user_quota_bytes: 150,
        allowed_extensions: vec!["txt".into()],
        ..Files::default()
    };

    // A permitted upload records metadata and assigns a storage path.
    let entry = files::add_file(&pool, uploads.id, &alice, "notes.txt", "my notes", 50, &cfg)
        .await
        .unwrap();
    assert_eq!(entry.storage_path, format!("{}-notes.txt", entry.id));
    assert_eq!(files::user_usage(&pool, alice.id).await.unwrap(), 50);

    // Extension allowlist and per-file size cap are enforced.
    assert!(matches!(
        files::add_file(&pool, uploads.id, &alice, "pic.png", "", 10, &cfg).await,
        Err(AppError::ExtensionNotAllowed)
    ));
    assert!(matches!(
        files::add_file(&pool, uploads.id, &alice, "big.txt", "", 101, &cfg).await,
        Err(AppError::FileTooLarge(_))
    ));

    // Quota is enforced across the running total (50 used + 50 = 100 ok; +60 over).
    files::add_file(&pool, uploads.id, &alice, "more.txt", "", 50, &cfg)
        .await
        .unwrap();
    assert!(matches!(
        files::add_file(&pool, uploads.id, &alice, "third.txt", "", 60, &cfg).await,
        Err(AppError::QuotaExceeded(_))
    ));
    assert_eq!(files::user_usage(&pool, alice.id).await.unwrap(), 100);

    // Admins bypass the quota (operator seeding is effectively an admin).
    auth::register_user(&pool, "adminuser", "pw", &Default::default())
        .await
        .unwrap();
    bbs_rs::services::admin::set_role(&pool, "adminuser", "admin")
        .await
        .unwrap();
    let admin = auth::find_user(&pool, "adminuser").await.unwrap().unwrap();
    // Two 100-byte files total 200 bytes, past the 150-byte quota; both still
    // succeed because the admin is exempt.
    files::add_file(&pool, uploads.id, &admin, "a.txt", "", 100, &cfg)
        .await
        .unwrap();
    files::add_file(&pool, uploads.id, &admin, "b.txt", "", 100, &cfg)
        .await
        .expect("admins are not quota-limited");
    assert_eq!(files::user_usage(&pool, admin.id).await.unwrap(), 200);

    // Descriptions can be set after the fact (e.g. an SFTP upload had none).
    assert!(
        files::set_description(&pool, entry.id, "now described")
            .await
            .unwrap()
    );
    assert_eq!(
        files::get_file(&pool, entry.id).await.unwrap().description,
        "now described"
    );
    assert!(!files::set_description(&pool, 99999, "x").await.unwrap());

    // Deleting a file returns its storage path (for the caller to unlink) and
    // frees quota.
    let path = files::delete_file(&pool, entry.id).await.unwrap();
    assert_eq!(path, Some(format!("{}-notes.txt", entry.id)));
    assert_eq!(files::user_usage(&pool, alice.id).await.unwrap(), 50);

    // Read ACL: an admin-only area is hidden from lower roles.
    files::add_area(&pool, "Staff", "internal", Some("admin"), Some("admin"))
        .await
        .unwrap();
    let user_view = files::list_readable_areas(&pool, "user").await.unwrap();
    assert!(!user_view.iter().any(|a| a.name == "Staff"));
    assert!(user_view.iter().any(|a| a.name == "Uploads"));
    let admin_view = files::list_readable_areas(&pool, "admin").await.unwrap();
    assert!(admin_view.iter().any(|a| a.name == "Staff"));

    // An area with files can't be deleted until it's empty.
    assert!(files::delete_area(&pool, "Uploads").await.is_err());
    assert!(files::delete_area(&pool, "Staff").await.unwrap());
    assert!(!files::delete_area(&pool, "Staff").await.unwrap());

    // Invalid roles are rejected when creating areas.
    assert!(matches!(
        files::add_area(&pool, "Bad", "", Some("wizard"), None).await,
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
        bbs_rs::services::mail::send_mail(&pool, &alice, "nobody", "s", "b", &Default::default())
            .await,
        Err(bbs_rs::error::AppError::RecipientNotFound)
    ));

    bbs_rs::services::mail::send_mail(&pool, &alice, "bob", "Hi Bob", "hello", &Default::default())
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
        oneliners::add(
            &pool,
            &guest,
            "hi",
            &Default::default(),
            &Default::default()
        )
        .await,
        Err(bbs_rs::error::AppError::GuestNotAllowed)
    ));

    // Empty / whitespace-only and over-length bodies are rejected.
    assert!(matches!(
        oneliners::add(
            &pool,
            &alice,
            "   ",
            &Default::default(),
            &Default::default()
        )
        .await,
        Err(bbs_rs::error::AppError::OnelinerLength(_))
    ));
    let too_long = "x".repeat(oneliners::MAX_LEN + 1);
    assert!(matches!(
        oneliners::add(
            &pool,
            &alice,
            &too_long,
            &Default::default(),
            &Default::default()
        )
        .await,
        Err(bbs_rs::error::AppError::OnelinerLength(_))
    ));

    // A valid post is trimmed and stored.
    oneliners::add(
        &pool,
        &alice,
        "  first!  ",
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();
    oneliners::add(
        &pool,
        &alice,
        "second",
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();
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

/// The wall no longer auto-trims. This deliberately reverses #32: oneliners are
/// ActivityPub statuses now, and a federated post has a permanent URI — trimming
/// one out from under remote servers would orphan their references and demand
/// `Delete` fan-out. Moderation replaces the ring buffer.
#[tokio::test]
async fn oneliner_wall_keeps_everything() {
    use bbs_rs::config::{Limits, Oneliners};
    use bbs_rs::services::{auth, oneliners};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    // Rate limiting off so we can post freely.
    let limits = Limits {
        window_secs: 0,
        ..Default::default()
    };
    let cfg = Oneliners::default();

    for n in 1..=25 {
        oneliners::add(&pool, &alice, &n.to_string(), &limits, &cfg)
            .await
            .unwrap();
    }
    assert_eq!(
        oneliners::count(&pool).await.unwrap(),
        25,
        "posts must survive: their URIs are permanent once federated"
    );
    let bodies: Vec<String> = oneliners::recent(&pool, 3)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.body)
        .collect();
    assert_eq!(bodies, vec!["25", "24", "23"], "newest first");

    // Explicit deletion is the way a post leaves the wall now.
    let oldest = oneliners::recent(&pool, 100).await.unwrap().pop().unwrap();
    assert!(oneliners::delete(&pool, oldest.id).await.unwrap());
    assert_eq!(oneliners::count(&pool).await.unwrap(), 24);
}

/// The length cap is raised to Mastodon's 500, not removed: "like a federated
/// post" means a server-defined limit, and remote servers reject oversized
/// payloads.
#[tokio::test]
async fn oneliner_length_cap_is_mastodon_sized() {
    use bbs_rs::config::{Limits, Oneliners};
    use bbs_rs::error::AppError;
    use bbs_rs::services::{auth, oneliners};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let limits = Limits {
        window_secs: 0,
        ..Default::default()
    };
    let cfg = Oneliners::default();
    assert_eq!(cfg.max_length, 500);

    // The old 120-char limit no longer bites.
    oneliners::add(&pool, &alice, &"x".repeat(300), &limits, &cfg)
        .await
        .unwrap();
    oneliners::add(&pool, &alice, &"x".repeat(500), &limits, &cfg)
        .await
        .unwrap();
    assert!(matches!(
        oneliners::add(&pool, &alice, &"x".repeat(501), &limits, &cfg).await,
        Err(AppError::OnelinerLength(500))
    ));

    // 0 still means unlimited, for operators who want it.
    let uncapped = Oneliners { max_length: 0 };
    oneliners::add(&pool, &alice, &"x".repeat(5000), &limits, &uncapped)
        .await
        .unwrap();
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

#[tokio::test]
async fn mail_unread_count_tracks_reads() {
    use bbs_rs::services::{auth, mail};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let limits = Default::default();

    // No mail yet.
    assert_eq!(mail::unread_count(&pool, bob.id).await.unwrap(), 0);

    // Alice sends bob two messages: both unread.
    mail::send_mail(&pool, &alice, "bob", "one", "hi", &limits)
        .await
        .unwrap();
    mail::send_mail(&pool, &alice, "bob", "two", "yo", &limits)
        .await
        .unwrap();
    assert_eq!(mail::unread_count(&pool, bob.id).await.unwrap(), 2);
    // The sender has nothing addressed to them.
    assert_eq!(mail::unread_count(&pool, alice.id).await.unwrap(), 0);

    // Reading one message marks it read and drops the count to 1.
    let inbox = mail::inbox(&pool, bob.id).await.unwrap();
    mail::read_mail(&pool, inbox[0].id, bob.id).await.unwrap();
    assert_eq!(mail::unread_count(&pool, bob.id).await.unwrap(), 1);

    // Re-reading the same message is idempotent for the count.
    mail::read_mail(&pool, inbox[0].id, bob.id).await.unwrap();
    assert_eq!(mail::unread_count(&pool, bob.id).await.unwrap(), 1);
}

// ---- #70: mail delete, reply/forward prefill --------------------------------

#[tokio::test]
async fn deleting_mail_is_scoped_to_the_recipient() {
    use bbs_rs::services::{auth, mail};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    mail::send_mail(&pool, &alice, "bob", "Hello", "hi bob", &Default::default())
        .await
        .unwrap();
    let m = mail::inbox(&pool, bob.id).await.unwrap().remove(0);

    // Alice (the sender, not the recipient) can't delete it from bob's mailbox.
    assert!(
        !mail::delete_mail(&pool, m.id, alice.id).await.unwrap(),
        "only the recipient can delete"
    );
    assert_eq!(mail::inbox(&pool, bob.id).await.unwrap().len(), 1);

    // Bob can.
    assert!(mail::delete_mail(&pool, m.id, bob.id).await.unwrap());
    assert!(mail::inbox(&pool, bob.id).await.unwrap().is_empty());

    // Deleting again is a no-op, not an error.
    assert!(!mail::delete_mail(&pool, m.id, bob.id).await.unwrap());
}

#[tokio::test]
async fn a_deleted_message_stops_counting_as_unread() {
    use bbs_rs::services::{auth, mail};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    mail::send_mail(&pool, &alice, "bob", "New", "unread", &Default::default())
        .await
        .unwrap();
    assert_eq!(mail::unread_count(&pool, bob.id).await.unwrap(), 1);

    let m = mail::inbox(&pool, bob.id).await.unwrap().remove(0);
    mail::delete_mail(&pool, m.id, bob.id).await.unwrap();
    assert_eq!(
        mail::unread_count(&pool, bob.id).await.unwrap(),
        0,
        "a deleted message isn't unread — it's gone"
    );
}

#[tokio::test]
async fn reply_prefills_recipient_subject_and_quoted_body() {
    use bbs_rs::services::{auth, mail};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    mail::send_mail(
        &pool,
        &alice,
        "bob",
        "Question",
        "line one\nline two",
        &Default::default(),
    )
    .await
    .unwrap();
    let m = mail::inbox(&pool, bob.id).await.unwrap().remove(0);

    let (to, subject, body) = mail::reply_prefill(&m);
    assert_eq!(to, "alice", "reply goes to the original sender");
    assert_eq!(subject, "Re: Question");
    assert!(body.contains("alice wrote:"), "attribution line: {body}");
    assert!(body.contains("> line one"), "original quoted: {body}");
    assert!(body.contains("> line two"));
    assert!(
        body.ends_with("\n\n"),
        "trailing blank line for the reply cursor"
    );

    // A reply to a reply doesn't stack "Re:".
    let mut m2 = m.clone();
    m2.subject = "Re: Question".into();
    assert_eq!(mail::reply_prefill(&m2).1, "Re: Question");
}

#[tokio::test]
async fn forward_prefills_subject_and_verbatim_body_without_a_recipient() {
    use bbs_rs::services::{auth, mail};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    mail::send_mail(
        &pool,
        &alice,
        "bob",
        "Notice",
        "the original text",
        &Default::default(),
    )
    .await
    .unwrap();
    let m = mail::inbox(&pool, bob.id).await.unwrap().remove(0);

    let (subject, body) = mail::forward_prefill(&m);
    assert_eq!(subject, "Fwd: Notice");
    assert!(body.contains("Forwarded message"), "{body}");
    assert!(body.contains("From: alice"));
    assert!(
        body.contains("the original text"),
        "body verbatim, not quoted"
    );
    assert!(!body.contains("> the original"), "forward doesn't quote");

    // Forwarding a forward doesn't stack "Fwd:".
    let mut m2 = m.clone();
    m2.subject = "Fwd: Notice".into();
    assert_eq!(mail::forward_prefill(&m2).0, "Fwd: Notice");
}

// ---- #92: author edit/delete own posts --------------------------------------

#[tokio::test]
async fn an_author_can_edit_their_own_post() {
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        &alice,
        "Subj",
        "original",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        boards::edit_own_message(&pool, id, &alice, "New subj", "edited", &Default::default())
            .await
            .unwrap()
    );
    let m = boards::get_message(&pool, id).await.unwrap();
    assert_eq!(m.subject, "New subj");
    assert_eq!(m.body, "edited");
    assert!(m.edited_at.is_some(), "edit stamps edited_at");

    // The edit reaches full-text search too (the FTS update trigger).
    let hits = bbs_rs::services::search::search_messages(&pool, "alice", "edited", 10)
        .await
        .unwrap();
    assert!(hits.iter().any(|h| h.id == id), "edited body is searchable");
    let stale = bbs_rs::services::search::search_messages(&pool, "alice", "original", 10)
        .await
        .unwrap();
    assert!(
        !stale.iter().any(|h| h.id == id),
        "old body no longer matches"
    );
}

#[tokio::test]
async fn a_user_cannot_edit_someone_elses_post() {
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        &alice,
        "S",
        "alice's",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        !boards::edit_own_message(&pool, id, &bob, "hijack", "bob's", &Default::default())
            .await
            .unwrap(),
        "bob can't edit alice's post"
    );
    assert_eq!(
        boards::get_message(&pool, id).await.unwrap().body,
        "alice's"
    );
}

#[tokio::test]
async fn an_author_can_delete_their_own_post_but_not_anothers() {
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let bob = auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        &alice,
        "S",
        "body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        !boards::delete_own_message(&pool, id, &bob).await.unwrap(),
        "not bob's to delete"
    );
    assert!(boards::delete_own_message(&pool, id, &alice).await.unwrap());
    assert!(
        boards::list_messages(&pool, board.id)
            .await
            .unwrap()
            .is_empty(),
        "gone once the author deletes it"
    );
}

#[tokio::test]
async fn a_locked_board_blocks_author_edit_and_delete() {
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        &alice,
        "S",
        "body",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    boards::set_locked(&pool, board.id, true).await.unwrap();

    assert!(
        !boards::edit_own_message(&pool, id, &alice, "S", "changed", &Default::default())
            .await
            .unwrap(),
        "a locked board freezes even the author's own posts"
    );
    assert!(!boards::delete_own_message(&pool, id, &alice).await.unwrap());
    // Admins still moderate a locked board — the ungated path ignores the lock.
    assert!(boards::delete_message(&pool, id).await.unwrap());
}

#[tokio::test]
async fn an_over_long_edit_is_refused() {
    use bbs_rs::config::Limits;
    use bbs_rs::services::{auth, boards};
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::list_boards(&pool).await.unwrap().remove(0);
    let id = boards::post_message(
        &pool,
        board.id,
        &alice,
        "S",
        "ok",
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    let limits = Limits {
        max_body_chars: 5,
        ..Default::default()
    };

    assert!(
        boards::edit_own_message(&pool, id, &alice, "S", "way too long", &limits)
            .await
            .is_err(),
        "the length cap applies to edits, not just new posts"
    );
    assert_eq!(
        boards::get_message(&pool, id).await.unwrap().body,
        "ok",
        "unchanged"
    );
}
