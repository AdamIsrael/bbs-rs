//! Integration tests for access control: the login decision (`attempt_login`)
//! with ban enforcement + audit logging, and the admin service operations.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::error::AppError;
use bbs_rs::services::{admin, auth, seed};

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
    seed(&pool).await.expect("seed");
    pool
}

#[tokio::test]
async fn attempt_login_accepts_records_and_enforces_bans() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();

    // Good credentials → Some, and a success row is logged.
    let ok = auth::attempt_login(&pool, "alice", "pw", Some("1.2.3.4"))
        .await
        .unwrap();
    assert!(ok.is_some());

    // Wrong password → None, and a failure row is logged.
    let bad = auth::attempt_login(&pool, "alice", "nope", Some("1.2.3.4"))
        .await
        .unwrap();
    assert!(bad.is_none());

    // Banned account → rejected even with the right password.
    admin::ban_user(&pool, "alice").await.unwrap();
    let banned = auth::attempt_login(&pool, "alice", "pw", Some("1.2.3.4"))
        .await
        .unwrap();
    assert!(banned.is_none(), "banned user must be rejected");
    admin::unban_user(&pool, "alice").await.unwrap();

    // Banned IP → rejected before password is even checked.
    admin::ban_ip(&pool, "9.9.9.9", "abuse", None)
        .await
        .unwrap();
    let ip_blocked = auth::attempt_login(&pool, "alice", "pw", Some("9.9.9.9"))
        .await
        .unwrap();
    assert!(ip_blocked.is_none(), "banned IP must be rejected");

    // All four attempts above were recorded, with exactly one success.
    let logins = admin::recent_logins(&pool, None, 100).await.unwrap();
    assert_eq!(logins.len(), 4);
    assert_eq!(logins.iter().filter(|l| l.success).count(), 1);
    assert_eq!(logins.iter().filter(|l| !l.success).count(), 3);
}

#[tokio::test]
async fn ban_unban_user_toggles_state() {
    let pool = setup().await;
    auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();

    assert!(
        !auth::find_user(&pool, "bob")
            .await
            .unwrap()
            .unwrap()
            .is_banned()
    );
    admin::ban_user(&pool, "bob").await.unwrap();
    assert!(
        auth::find_user(&pool, "bob")
            .await
            .unwrap()
            .unwrap()
            .is_banned()
    );
    admin::unban_user(&pool, "bob").await.unwrap();
    assert!(
        !auth::find_user(&pool, "bob")
            .await
            .unwrap()
            .unwrap()
            .is_banned()
    );
}

#[tokio::test]
async fn ip_ban_lifecycle() {
    let pool = setup().await;
    assert!(!admin::is_ip_banned(&pool, "5.6.7.8").await.unwrap());
    admin::ban_ip(&pool, "5.6.7.8", "spam", None).await.unwrap();
    assert!(admin::is_ip_banned(&pool, "5.6.7.8").await.unwrap());
    assert_eq!(admin::list_ip_bans(&pool).await.unwrap().len(), 1);
    let banned = admin::banned_ips(&pool).await.unwrap();
    assert!(banned.contains("5.6.7.8"));
    admin::unban_ip(&pool, "5.6.7.8").await.unwrap();
    assert!(!admin::is_ip_banned(&pool, "5.6.7.8").await.unwrap());
}

#[tokio::test]
async fn expired_ip_ban_is_inactive_and_purgeable() {
    let pool = setup().await;
    let now = bbs_rs::util::now_unix();

    // Permanent ban is active; already-expired ban is not.
    admin::ban_ip(&pool, "1.1.1.1", "manual", None)
        .await
        .unwrap();
    admin::ban_ip(&pool, "2.2.2.2", "auto", Some(now - 1))
        .await
        .unwrap();
    // Future expiry is still active.
    admin::ban_ip(&pool, "3.3.3.3", "auto", Some(now + 3600))
        .await
        .unwrap();

    assert!(admin::is_ip_banned(&pool, "1.1.1.1").await.unwrap());
    assert!(!admin::is_ip_banned(&pool, "2.2.2.2").await.unwrap());
    assert!(admin::is_ip_banned(&pool, "3.3.3.3").await.unwrap());

    // list/banned_ips exclude the expired one.
    let active = admin::banned_ips(&pool).await.unwrap();
    assert!(active.contains("1.1.1.1") && active.contains("3.3.3.3"));
    assert!(!active.contains("2.2.2.2"));

    // Purge removes the expired ban only.
    let removed = admin::purge_expired_ip_bans(&pool).await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(admin::list_ip_bans(&pool).await.unwrap().len(), 2);
}

#[tokio::test]
async fn failure_threshold_detects_offenders() {
    let pool = setup().await;
    let now = bbs_rs::util::now_unix();

    // 3 failures from a bot IP, 1 from another, plus a success (shouldn't count).
    for _ in 0..3 {
        admin::record_login(&pool, "root", Some("6.6.6.6"), false)
            .await
            .unwrap();
    }
    admin::record_login(&pool, "alice", Some("7.7.7.7"), false)
        .await
        .unwrap();
    admin::record_login(&pool, "carol", Some("6.6.6.6"), true)
        .await
        .unwrap();

    // Threshold 3 within the window catches only 6.6.6.6.
    let offenders = admin::ips_over_failure_threshold(&pool, 3, now - 600)
        .await
        .unwrap();
    assert_eq!(offenders, vec!["6.6.6.6".to_string()]);

    // Already-banned IPs are excluded from future detection.
    admin::ban_ip(&pool, "6.6.6.6", "auto", Some(now + 3600))
        .await
        .unwrap();
    let again = admin::ips_over_failure_threshold(&pool, 3, now - 600)
        .await
        .unwrap();
    assert!(again.is_empty(), "banned IP is not re-detected");

    // Old failures outside the window don't count.
    let none = admin::ips_over_failure_threshold(&pool, 3, now + 1)
        .await
        .unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn set_role_validates_and_promotes() {
    let pool = setup().await;
    auth::register_user(&pool, "carol", "pw", &Default::default())
        .await
        .unwrap();

    admin::set_role(&pool, "carol", "admin").await.unwrap();
    let carol = auth::find_user(&pool, "carol").await.unwrap().unwrap();
    assert!(carol.is_admin());

    // Bogus role is rejected.
    assert!(matches!(
        admin::set_role(&pool, "carol", "wizard").await,
        Err(AppError::BadRole(_))
    ));
    // Unknown user is a not-found.
    assert!(matches!(
        admin::set_role(&pool, "ghost", "admin").await,
        Err(AppError::NotFound)
    ));
}

#[tokio::test]
async fn recent_logins_filters_and_limits() {
    let pool = setup().await;
    for _ in 0..3 {
        admin::record_login(&pool, "dave", Some("1.1.1.1"), false)
            .await
            .unwrap();
    }
    admin::record_login(&pool, "erin", Some("2.2.2.2"), true)
        .await
        .unwrap();

    // Filter by user.
    let dave = admin::recent_logins(&pool, Some("dave"), 100)
        .await
        .unwrap();
    assert_eq!(dave.len(), 3);
    assert!(dave.iter().all(|l| l.username == "dave" && !l.success));

    // Limit applies (newest first).
    let capped = admin::recent_logins(&pool, None, 2).await.unwrap();
    assert_eq!(capped.len(), 2);

    // banned_usernames reflects a ban.
    admin::ban_user(&pool, "dave").await.ok();
    // (dave isn't a real user here, so ban_user is a no-op; register + ban:)
    auth::register_user(&pool, "frank", "pw", &Default::default())
        .await
        .unwrap();
    admin::ban_user(&pool, "frank").await.unwrap();
    assert!(
        admin::banned_usernames(&pool)
            .await
            .unwrap()
            .contains("frank")
    );
}

#[tokio::test]
async fn list_users_includes_seeded_guest() {
    let pool = setup().await;
    let users = admin::list_users(&pool).await.unwrap();
    assert!(users.iter().any(|u| u.username == "guest" && u.is_guest()));
}
