//! Text templating (#89): operator strings — the MOTD/welcome banner,
//! bulletins, and menu labels — are rendered against the session context
//! (transport, identity, live counts) before display.

use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::{App, ui};
use bbs_rs::config::Settings;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{self, auth};
use bbs_rs::transport::Transport;

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

async fn app_with(settings: Settings, transport: Transport) -> App {
    let pool = setup().await;
    let alice = auth::register_user(&pool, "alice", "pw", &Default::default())
        .await
        .unwrap();
    App::new(
        pool,
        Presence::new(),
        Arc::new(settings),
        alice,
        1,
        transport,
    )
}

fn dump(app: &App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    term.backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

#[tokio::test]
async fn render_text_substitutes_session_context() {
    let app = app_with(Settings::default(), Transport::Ssh).await;

    // Identity + a transport conditional.
    assert_eq!(
        app.render_text("Hi {{user}} on {{transport}}"),
        "Hi alice on ssh"
    );
    assert_eq!(
        app.render_text("{{#if ssh}}via SSH{{else}}via web{{/if}}"),
        "via SSH"
    );
    // bbs_name comes from config; an unknown var renders empty.
    assert_eq!(app.render_text("{{bbs_name}}{{nope}}"), "bbs-rs");
}

#[tokio::test]
async fn transport_flips_the_web_and_ssh_flags() {
    let web = app_with(Settings::default(), Transport::Web).await;
    assert_eq!(
        web.render_text("{{#if web}}browser{{else}}terminal{{/if}}"),
        "browser"
    );
    assert_eq!(web.render_text("{{transport}}"), "web");
}

#[tokio::test]
async fn a_templated_motd_renders_on_the_main_menu() {
    // A conditional + a variable in the welcome banner.
    let settings = Settings {
        bbs: bbs_rs::config::Bbs {
            welcome: "Hello {{user}}{{#if ssh}} — SSH session{{/if}}".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let app = app_with(settings, Transport::Ssh).await;

    let screen = dump(&app, 80, 24);
    assert!(
        screen.contains("Hello alice — SSH session"),
        "the MOTD template was rendered on the menu; got:\n{screen}"
    );
    // The raw template braces never reach the screen.
    assert!(!screen.contains("{{"), "no unrendered tags leak through");
}

#[tokio::test]
async fn a_templated_menu_label_renders() {
    let settings = Settings {
        menu: vec![
            bbs_rs::config::MenuEntry {
                action: "who".into(),
                label: "Online now: {{who_online}}".into(),
                key: "w".into(),
                row: None,
                col: None,
            },
            bbs_rs::config::MenuEntry {
                action: "quit".into(),
                label: "".into(),
                key: "".into(),
                row: None,
                col: None,
            },
        ],
        ..Default::default()
    };
    let mut app = app_with(settings, Transport::Ssh).await;
    app.online_count = 4;

    let screen = dump(&app, 80, 24);
    assert!(
        screen.contains("Online now: 4"),
        "the menu label template was rendered; got:\n{screen}"
    );
}
