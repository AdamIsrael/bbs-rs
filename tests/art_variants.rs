//! Context-conditional art (#90): `[[art.variants]]` swap a screen's file per
//! session — the first variant whose `when` flag is truthy wins, falling back
//! to the `welcome`/`screens` default (and past a missing variant file).

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use bbs_rs::app::App;
use bbs_rs::app::state::Screen;
use bbs_rs::config::{Art, ArtVariant, Settings};
use bbs_rs::db::models::User;
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

/// A fresh temp art dir seeded with `(filename, contents)` pairs. Unique per
/// test name so parallel tests don't collide.
fn art_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bbs-art-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, contents) in files {
        std::fs::write(dir.join(file), contents).unwrap();
    }
    dir
}

fn variant(screen: &str, when: &str, file: &str) -> ArtVariant {
    ArtVariant {
        screen: screen.into(),
        when: when.into(),
        file: file.into(),
    }
}

async fn app_with(art: Art, transport: Transport, user: User) -> App {
    let pool = setup().await;
    let settings = Settings {
        art,
        ..Default::default()
    };
    App::new(
        pool,
        Presence::new(),
        Arc::new(settings),
        user,
        1,
        transport,
    )
}

async fn reg_user(name: &str) -> (SqlitePool, User) {
    let pool = setup().await;
    let u = auth::register_user(&pool, name, "pw", &Default::default())
        .await
        .unwrap();
    (pool, u)
}

/// Flatten a screen's loaded art back into a plain string for assertions.
fn art_text(app: &App, screen: Screen) -> String {
    app.art
        .get(&screen)
        .map(|t| {
            t.lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn art(dir: PathBuf, welcome: &str, variants: Vec<ArtVariant>) -> Art {
    Art {
        dir,
        welcome: welcome.into(),
        variants,
        ..Default::default()
    }
}

#[tokio::test]
async fn transport_selects_the_matching_variant() {
    let dir = art_dir(
        "transport",
        &[("menu.ans", "DEFAULT"), ("menu-web.ans", "WEBART")],
    );
    let make = |t| {
        let dir = dir.clone();
        async move {
            let (_pool, alice) = reg_user("alice").await;
            app_with(
                art(
                    dir,
                    "menu.ans",
                    vec![variant("main_menu", "web", "menu-web.ans")],
                ),
                t,
                alice,
            )
            .await
        }
    };

    let web = make(Transport::Web).await;
    assert!(
        art_text(&web, Screen::MainMenu).contains("WEBART"),
        "web session gets the web variant"
    );

    let ssh = make(Transport::Ssh).await;
    assert!(
        art_text(&ssh, Screen::MainMenu).contains("DEFAULT"),
        "ssh session (no matching variant) gets the default"
    );
}

#[tokio::test]
async fn a_missing_variant_file_falls_back_to_the_default() {
    let dir = art_dir("missing", &[("menu.ans", "DEFAULT")]);
    let (_pool, alice) = reg_user("alice").await;
    // The variant matches (web) but its file doesn't exist → fall back.
    let app = app_with(
        art(
            dir,
            "menu.ans",
            vec![variant("main_menu", "web", "nope.ans")],
        ),
        Transport::Web,
        alice,
    )
    .await;
    assert!(
        art_text(&app, Screen::MainMenu).contains("DEFAULT"),
        "a matched-but-missing variant falls back to the default file"
    );
}

#[tokio::test]
async fn the_first_matching_variant_wins() {
    let dir = art_dir("first", &[("a.ans", "FIRST"), ("b.ans", "SECOND")]);
    let (_pool, alice) = reg_user("alice").await;
    // Both match an ssh session; array order decides.
    let app = app_with(
        art(
            dir,
            "",
            vec![
                variant("main_menu", "ssh", "a.ans"),
                variant("main_menu", "ssh", "b.ans"),
            ],
        ),
        Transport::Ssh,
        alice,
    )
    .await;
    assert!(art_text(&app, Screen::MainMenu).contains("FIRST"));
    assert!(!art_text(&app, Screen::MainMenu).contains("SECOND"));
}

#[tokio::test]
async fn a_variant_only_screen_needs_no_default() {
    let dir = art_dir("only", &[("boards-web.ans", "WEBBOARDS")]);
    let (_pool, alice) = reg_user("alice").await;
    // No default board_list art at all — just a web variant.
    let app = app_with(
        art(
            dir,
            "",
            vec![variant("board_list", "web", "boards-web.ans")],
        ),
        Transport::Web,
        alice,
    )
    .await;
    assert!(
        art_text(&app, Screen::BoardList).contains("WEBBOARDS"),
        "a variant can add art to a screen with no configured default"
    );
    // ...and an ssh session, with no match and no default, gets nothing.
    let (_pool, bob) = reg_user("bob").await;
    let dir2 = art_dir("only2", &[("boards-web.ans", "WEBBOARDS")]);
    let ssh = app_with(
        art(
            dir2,
            "",
            vec![variant("board_list", "web", "boards-web.ans")],
        ),
        Transport::Ssh,
        bob,
    )
    .await;
    assert!(!ssh.art.contains_key(&Screen::BoardList));
}

#[tokio::test]
async fn a_role_flag_gates_the_variant() {
    let dir = art_dir(
        "role",
        &[("menu.ans", "DEFAULT"), ("menu-guest.ans", "GUESTART")],
    );
    let variants = vec![variant("main_menu", "guest", "menu-guest.ans")];

    // A guest matches the guest variant.
    let pool = setup().await;
    let guest = auth::find_user(&pool, "guest").await.unwrap().unwrap();
    let g = app_with(
        art(dir.clone(), "menu.ans", variants.clone()),
        Transport::Ssh,
        guest,
    )
    .await;
    assert!(art_text(&g, Screen::MainMenu).contains("GUESTART"));

    // A registered user does not → default.
    let (_pool, alice) = reg_user("alice").await;
    let a = app_with(art(dir, "menu.ans", variants), Transport::Ssh, alice).await;
    assert!(art_text(&a, Screen::MainMenu).contains("DEFAULT"));
}
