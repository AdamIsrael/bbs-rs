//! End-to-end render checks: draw a real `App` to a ratatui `TestBackend` and
//! assert the resolved theme color reaches the title bar and that configured
//! welcome art is rendered into the buffer.

use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use bbs_rs::app::App;
use bbs_rs::app::ui;
use bbs_rs::config::Settings;
use bbs_rs::services::{self, auth, presence::Presence};

async fn guest_pool() -> (sqlx::SqlitePool, bbs_rs::db::models::User) {
    use sqlx::sqlite::SqlitePoolOptions;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    services::seed(&pool, &Default::default()).await.unwrap();
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    (pool, guest)
}

fn dump(buf: &Buffer) -> String {
    let area = *buf.area();
    let mut s = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            s.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        s.push('\n');
    }
    s
}

#[tokio::test]
async fn theme_color_reaches_title_bar() {
    let (pool, guest) = guest_pool().await;
    let mut settings = Settings::default();
    settings.theme.preset = Some("amber".into()); // title_bg = Rgb(255,176,0)
    let app = App::new(pool, Presence::new(), Arc::new(settings), guest, 1);

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| ui::draw(f, &app)).unwrap();
    let buf = term.backend().buffer();

    // The top row is the title bar; its background should be the amber theme.
    assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(255, 176, 0));
}

#[tokio::test]
async fn welcome_art_is_rendered() {
    let (pool, guest) = guest_pool().await;

    // Write a welcome-art file into a temp dir and point the config at it.
    let dir = std::env::temp_dir().join("bbs_rs_art_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("welcome.ans"), b"\x1b[32mXYZZY-ART\x1b[0m").unwrap();

    let mut settings = Settings::default();
    settings.art.dir = dir;
    settings.art.welcome = "welcome.ans".into();
    let app = App::new(pool, Presence::new(), Arc::new(settings), guest, 1);

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| ui::draw(f, &app)).unwrap();
    let screen = dump(term.backend().buffer());

    assert!(
        screen.contains("XYZZY-ART"),
        "welcome art should be rendered; got:\n{screen}"
    );
}
