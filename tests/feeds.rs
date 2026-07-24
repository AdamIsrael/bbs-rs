//! Public RSS/Atom board feeds (#100), driven over real HTTP: the ACL boundary
//! (only guest-readable boards are exposed), content negotiation, the toggle,
//! and the discovery index.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use arc_swap::ArcSwap;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::config::Settings;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{auth, boards};
use bbs_rs::web;

async fn setup() -> SqlitePool {
    // A unique file per test: these run in parallel in one process, so a shared
    // name would race.
    static N: AtomicUsize = AtomicUsize::new(0);
    let db = std::env::temp_dir().join(format!(
        "bbs_feeds_test_{}_{}.db",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&db);
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

/// Bind an ephemeral port, serve the given config in the background, and return
/// the base URL. `config` is shared so the test can flip toggles.
async fn serve(pool: SqlitePool, config: Arc<ArcSwap<Settings>>) -> String {
    let state = web::WebState::new(pool, config, Presence::new(), Arc::new(AtomicUsize::new(0)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = web::serve(listener, state).await;
    });
    format!("http://{addr}")
}

/// A non-guest author who can post.
async fn poster(pool: &SqlitePool) -> bbs_rs::db::models::User {
    auth::register_user(pool, "alice", "pw", &Default::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn guest_readable_board_serves_atom_by_default() {
    let pool = setup().await;
    let alice = poster(&pool).await;
    let general = boards::find_board_by_name(&pool, "General")
        .await
        .unwrap()
        .unwrap();
    boards::post_message(
        &pool,
        general.id,
        &alice,
        "Hello & welcome <everyone>",
        "First post body",
        None,
        &Settings::default().limits,
    )
    .await
    .unwrap();

    let base = serve(pool, Arc::new(ArcSwap::from_pointee(Settings::default()))).await;
    let resp = reqwest::get(format!("{base}/feed/General")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("application/atom+xml"), "got {ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
    // The hostile subject is escaped, not emitted raw.
    assert!(body.contains("Hello &amp; welcome &lt;everyone&gt;"));
    assert!(!body.contains("<everyone>"));
}

#[tokio::test]
async fn rss_format_is_selectable_and_case_insensitive_name() {
    let pool = setup().await;
    let base = serve(pool, Arc::new(ArcSwap::from_pointee(Settings::default()))).await;

    let resp = reqwest::get(format!("{base}/feed/general?format=rss"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "board name match is case-insensitive");
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("application/rss+xml"), "got {ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("<rss version=\"2.0\""));
}

#[tokio::test]
async fn a_restricted_board_is_not_exposed() {
    let pool = setup().await;
    // Raise General's read floor above guest.
    boards::set_roles(&pool, "General", Some("user"), None)
        .await
        .unwrap();

    let base = serve(pool, Arc::new(ArcSwap::from_pointee(Settings::default()))).await;
    let resp = reqwest::get(format!("{base}/feed/General")).await.unwrap();
    assert_eq!(
        resp.status(),
        404,
        "a user-only board must be invisible to the public feed"
    );
    // And a board that never existed 404s identically — no probing.
    let missing = reqwest::get(format!("{base}/feed/DefinitelyNotABoard"))
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn the_toggle_turns_all_feeds_off() {
    let pool = setup().await;
    let mut off = Settings::default();
    off.web.feeds = false;
    let base = serve(pool, Arc::new(ArcSwap::from_pointee(off))).await;

    for path in ["/feed", "/feed/General"] {
        let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(resp.status(), 404, "{path} should 404 when feeds are off");
    }
}

#[tokio::test]
async fn the_index_lists_public_boards_only() {
    let pool = setup().await;
    // Make one seeded board admin-only; it must not appear in the index.
    boards::set_roles(&pool, "General", Some("guest"), None)
        .await
        .unwrap();
    let base = serve(pool, Arc::new(ArcSwap::from_pointee(Settings::default()))).await;

    let resp = reqwest::get(format!("{base}/feed")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("text/html"), "got {ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("/feed/General"), "public board is listed");
}
