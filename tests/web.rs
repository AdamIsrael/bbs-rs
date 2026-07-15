//! End-to-end check of the browser transport: bind the axum server on an
//! ephemeral port, connect a real WebSocket client, log in as guest, and
//! confirm the app draws (ANSI bytes arrive) — proving auth + app::run + the
//! WebTerminalHandle output path all work over the web seam.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use futures_util::{SinkExt, StreamExt};
use sqlx::sqlite::SqlitePoolOptions;
use tokio_tungstenite::tungstenite::Message;

use bbs_rs::config::Settings;
use bbs_rs::services::presence::Presence;
use bbs_rs::web;

#[tokio::test]
async fn web_guest_login_renders() {
    // Shared file-backed DB so the server task and setup see the same data.
    let db = std::env::temp_dir().join(format!("bbs_web_test_{}.db", std::process::id()));
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

    let state = web::WebState::new(
        pool,
        Arc::new(Settings::default()),
        Presence::new(),
        Arc::new(AtomicUsize::new(0)),
    );

    // Bind an ephemeral port and serve in the background.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = web::serve(listener, state).await;
    });

    // Connect and log in as guest.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("ws connect");
    ws.send(Message::Text(
        r#"{"user":"guest","pass":"guest"}"#.to_string().into(),
    ))
    .await
    .unwrap();
    ws.send(Message::Text(
        r#"{"type":"resize","cols":80,"rows":24}"#.to_string().into(),
    ))
    .await
    .unwrap();

    // The app's first draw should arrive as binary frames of ANSI bytes.
    let mut got = Vec::new();
    for _ in 0..40 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                got.extend_from_slice(&b);
                if got.len() > 32 {
                    break;
                }
            }
            Ok(Some(Ok(Message::Text(t)))) => panic!("login rejected: {t}"),
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert!(
        !got.is_empty(),
        "expected the app to render ANSI output over the websocket"
    );
    // ratatui output contains ANSI CSI escapes (ESC[).
    assert!(
        got.windows(2).any(|w| w == b"\x1b["),
        "output should contain ANSI escape sequences"
    );

    let _ = std::fs::remove_file(&db);
}
