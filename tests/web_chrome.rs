//! Operator-customizable header/footer on the browser frontend (#201), driven
//! over real HTTP: the chrome appears, the tab title is the board's name, and —
//! the load-bearing part — `{{variable}}` substitutions are HTML-escaped while
//! the operator's own markup is not.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use arc_swap::ArcSwap;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::config::Settings;
use bbs_rs::services::presence::Presence;
use bbs_rs::web;

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

/// A scratch directory for chrome fragments, cleaned up on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bbs-chrome-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Write a fragment and return its path as a config string.
    fn write(&self, name: &str, body: &str) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn serve(pool: SqlitePool, config: Settings) -> String {
    let state = web::WebState::new(
        pool,
        Arc::new(ArcSwap::from_pointee(config)),
        Presence::new(),
        Arc::new(AtomicUsize::new(0)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = web::serve(listener, state).await;
    });
    format!("http://{addr}")
}

async fn page(base: &str) -> String {
    reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn an_unconfigured_board_renders_no_chrome() {
    let pool = setup().await;
    let base = serve(pool, Settings::default()).await;
    let body = page(&base).await;

    assert!(
        !body.contains("<header id=\"site-header\">"),
        "no header element (the CSS rule naming it always ships): {body}"
    );
    assert!(
        !body.contains("<footer id=\"site-footer\">"),
        "no footer element"
    );
    // The placeholders must be substituted away, never shipped to the browser.
    assert!(!body.contains("__BBS_"), "placeholders are gone: {body}");
    // And the terminal is still there.
    assert!(body.contains("id=\"terminal\""));
}

#[tokio::test]
async fn the_tab_title_is_the_board_name() {
    let pool = setup().await;
    let mut cfg = Settings::default();
    cfg.bbs.name = "Adam's Board".into();
    let base = serve(pool, cfg).await;

    assert!(
        page(&base)
            .await
            .contains("<title>Adam&#39;s Board</title>")
    );
}

#[tokio::test]
async fn header_and_footer_are_rendered() {
    let pool = setup().await;
    let dir = Scratch::new("basic");
    let mut cfg = Settings::default();
    cfg.web.header_file = dir.write("h.html", "<b>Top</b>");
    cfg.web.footer_file = dir.write(
        "f.html",
        r#"<a href="https://github.com/AdamIsrael/bbs-rs">source</a>"#,
    );
    let base = serve(pool, cfg).await;
    let body = page(&base).await;

    // The operator's own markup is emitted verbatim — they meant it.
    assert!(
        body.contains("<header id=\"site-header\"><b>Top</b></header>"),
        "{body}"
    );
    assert!(
        body.contains(r#"<a href="https://github.com/AdamIsrael/bbs-rs">source</a>"#),
        "the repo link survives as a real link: {body}"
    );
    assert!(body.contains("<footer id=\"site-footer\">"));
}

/// **The security property.** Template *substitutions* are escaped even though
/// the surrounding markup isn't, so a value can never open a tag — no matter
/// which variables the context grows later.
#[tokio::test]
async fn substitutions_are_escaped_but_markup_is_not() {
    let pool = setup().await;
    let dir = Scratch::new("escape");
    let mut cfg = Settings::default();
    // A board name that would be an injection if pasted in raw.
    cfg.bbs.name = "<script>alert(1)</script>".into();
    cfg.web.header_file = dir.write("h.html", "<span>{{bbs_name}}</span>");
    let base = serve(pool, cfg).await;
    let body = page(&base).await;

    assert!(
        body.contains("<span>&lt;script&gt;alert(1)&lt;/script&gt;</span>"),
        "the substitution is escaped: {body}"
    );
    assert!(
        !body.contains("<script>alert(1)</script>"),
        "and the raw tag never reaches the page: {body}"
    );
    // The operator's own <span> is untouched — only substitutions are escaped.
    assert!(body.contains("<span>&lt;script"), "{body}");
}

/// A decoration must never take the login page down with it.
#[tokio::test]
async fn a_missing_file_degrades_to_no_chrome() {
    let pool = setup().await;
    let mut cfg = Settings::default();
    cfg.web.header_file = "/definitely/not/here.html".into();
    let base = serve(pool, cfg).await;

    let resp = reqwest::get(format!("{base}/")).await.unwrap();
    assert_eq!(resp.status(), 200, "the page still serves");
    let body = resp.text().await.unwrap();
    assert!(!body.contains("<header id=\"site-header\">"));
    assert!(body.contains("id=\"terminal\""), "terminal intact");
}

/// Files are read per request, so an edit shows up on the next load without a
/// restart or even a config reload.
#[tokio::test]
async fn editing_the_file_takes_effect_on_the_next_load() {
    let pool = setup().await;
    let dir = Scratch::new("reload");
    let path = dir.write("h.html", "<b>before</b>");
    let mut cfg = Settings::default();
    cfg.web.header_file = path.clone();
    let base = serve(pool, cfg).await;

    assert!(page(&base).await.contains("<b>before</b>"));
    std::fs::write(&path, "<b>after</b>").unwrap();
    let body = page(&base).await;
    assert!(body.contains("<b>after</b>"), "{body}");
    assert!(!body.contains("<b>before</b>"), "the old content is gone");
}
