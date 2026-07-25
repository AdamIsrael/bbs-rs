//! The Prometheus `/metrics` endpoint (#98), driven over real HTTP: the
//! exposition format, that the numbers track reality, and the toggle.

use std::sync::Arc;

use arc_swap::ArcSwap;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::config::Settings;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{admin, auth, boards};
use bbs_rs::web::metrics::{MetricsState, serve};

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

/// Bind an ephemeral port, serve in the background, return the base URL.
async fn serve_on(pool: SqlitePool, presence: Presence, config: Settings) -> String {
    let state = MetricsState::new(pool, presence, Arc::new(ArcSwap::from_pointee(config)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = serve(listener, state).await;
    });
    format!("http://{addr}")
}

fn enabled() -> Settings {
    let mut s = Settings::default();
    s.metrics.enabled = true;
    s
}

/// Pull one metric's value out of the exposition text.
fn value(body: &str, name: &str) -> i64 {
    body.lines()
        .find(|l| l.starts_with(&format!("{name} ")))
        .unwrap_or_else(|| panic!("no sample for {name} in:\n{body}"))
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn exposition_is_well_formed() {
    let pool = setup().await;
    let base = serve_on(pool, Presence::new(), enabled()).await;

    let resp = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        ct.starts_with("text/plain") && ct.contains("version=0.0.4"),
        "Prometheus needs its own content type, got {ct}"
    );
    let body = resp.text().await.unwrap();

    // Every family carries HELP and TYPE — a scraper drops one that doesn't.
    for name in [
        "bbs_sessions_online",
        "bbs_users_total",
        "bbs_posts_total",
        "bbs_mail_total",
        "bbs_logins_total",
        "bbs_login_failures_24h",
    ] {
        assert!(
            body.contains(&format!("# HELP {name} ")),
            "no HELP for {name}"
        );
        assert!(
            body.contains(&format!("# TYPE {name} ")),
            "no TYPE for {name}"
        );
    }
    // Counters and gauges are labelled as such, not all lumped together.
    assert!(body.contains("# TYPE bbs_posts_total counter"));
    assert!(body.contains("# TYPE bbs_sessions_online gauge"));
    // The build label is the one sample carrying a label pair.
    assert!(body.contains("bbs_build_version{version=\""), "{body}");
}

#[tokio::test]
async fn the_numbers_track_reality() {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    let board = boards::find_board_by_name(&pool, "General")
        .await
        .unwrap()
        .unwrap();
    boards::post_message(
        &pool,
        board.id,
        &alice,
        "hello",
        "world",
        None,
        &Settings::default().limits,
    )
    .await
    .unwrap();
    admin::record_login(&pool, "alice", Some("1.2.3.4"), true)
        .await
        .unwrap();
    admin::record_login(&pool, "mallory", Some("9.9.9.9"), false)
        .await
        .unwrap();
    admin::ban_ip(&pool, "9.9.9.9", "brute force", None)
        .await
        .unwrap();

    // Two live sessions, one of them in chat.
    let presence = Presence::new();
    let (tx1, _rx1) = mpsc::channel(4);
    let (tx2, _rx2) = mpsc::channel(4);
    presence.join(1, "alice".into(), None, tx1).await;
    presence.join(2, "bob".into(), None, tx2).await;
    presence.chat_join(1).await;

    let base = serve_on(pool, presence, enabled()).await;
    let body = reqwest::get(format!("{base}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(value(&body, "bbs_sessions_online"), 2);
    assert_eq!(value(&body, "bbs_chat_participants"), 1);
    // guest is seeded alongside alice.
    assert_eq!(value(&body, "bbs_users_total"), 2);
    assert_eq!(value(&body, "bbs_posts_total"), 1);
    assert_eq!(value(&body, "bbs_logins_total"), 1);
    assert_eq!(value(&body, "bbs_login_failures_total"), 1);
    assert_eq!(value(&body, "bbs_login_failures_24h"), 1);
    assert_eq!(value(&body, "bbs_ip_bans"), 1);
}

#[tokio::test]
async fn banned_and_pending_users_are_counted_separately() {
    let pool = setup().await;
    auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    auth::register_user(&pool, "bob", "pw", &Default::default())
        .await
        .unwrap();
    admin::ban_user(&pool, "bob").await.unwrap();
    // A pending registration (#73).
    let accounts = bbs_rs::config::Accounts {
        require_validation: true,
        ..Default::default()
    };
    auth::register_user(&pool, "carol", "pw", &accounts)
        .await
        .unwrap();

    let base = serve_on(pool, Presence::new(), enabled()).await;
    let body = reqwest::get(format!("{base}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(value(&body, "bbs_users_banned"), 1);
    assert_eq!(value(&body, "bbs_users_pending"), 1);
}

#[tokio::test]
async fn remote_actors_are_not_counted_as_members() {
    let pool = setup().await;
    sqlx::query(
        "INSERT INTO users (username, password_hash, role, created_at, validated_at, is_remote) \
         VALUES ('someone@remote.social', '!', 'user', 0, 0, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let base = serve_on(pool, Presence::new(), enabled()).await;
    let body = reqwest::get(format!("{base}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // Only the seeded guest — a discovered fediverse actor isn't a member.
    assert_eq!(value(&body, "bbs_users_total"), 1);
}

#[tokio::test]
async fn the_toggle_closes_the_endpoint() {
    let pool = setup().await;
    // Default settings have metrics off; the listener still answers /healthz so
    // "process up, metrics off" is distinguishable from "process down".
    let base = serve_on(pool, Presence::new(), Settings::default()).await;

    let resp = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(resp.status(), 404, "metrics are 404 while disabled");

    let health = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(health.status(), 200);
}
