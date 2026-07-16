//! End-to-end check of the browser transport: bind the axum server on an
//! ephemeral port, connect a real WebSocket client, log in as guest, and
//! confirm the app draws (ANSI bytes arrive) — proving auth + app::run + the
//! WebTerminalHandle output path all work over the web seam.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use futures_util::{SinkExt, StreamExt};
use sqlx::sqlite::SqlitePoolOptions;
use tokio_tungstenite::tungstenite::Message;

use arc_swap::ArcSwap;
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
        Arc::new(ArcSwap::from_pointee(Settings::default())),
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

/// The same guest-login round-trip, but over HTTPS/WSS with a self-signed cert:
/// proves `serve_tls` terminates TLS and the app still renders over `wss://`.
#[tokio::test]
async fn web_tls_guest_login_renders() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let dir = std::env::temp_dir().join(format!("bbs_web_tls_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("bbs.db");
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
        Arc::new(ArcSwap::from_pointee(Settings::default())),
        Presence::new(),
        Arc::new(AtomicUsize::new(0)),
    );

    // Generate a self-signed cert (SAN localhost) and resolve it via the same
    // `web::tls` path production uses (BYO cert files → TlsSetup::Rustls).
    let (cert_pem, key_pem) = web::tls::self_signed_pems(vec!["localhost".into()]).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();
    let web_cfg = bbs_rs::config::Web {
        tls_cert: cert_path.to_string_lossy().into_owned(),
        tls_key: key_path.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let tls = web::tls::resolve(&web_cfg).await.unwrap();

    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    std_listener.set_nonblocking(true).unwrap();
    let port = std_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = web::serve_tls(std_listener, state, tls).await;
    });

    // Client that trusts the generated cert.
    let mut roots = rustls::RootCertStore::empty();
    let mut rd = cert_pem.as_bytes();
    for der in rustls_pemfile::certs(&mut rd) {
        roots.add(der.unwrap()).unwrap();
    }
    let client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_cfg));

    // Give the acceptor a moment, then connect over wss:// (SAN = localhost).
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let ws_url = format!("wss://localhost:{port}/ws");
    let (mut ws, _) =
        tokio_tungstenite::connect_async_tls_with_config(ws_url, None, false, Some(connector))
            .await
            .expect("wss connect");
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
        got.windows(2).any(|w| w == b"\x1b["),
        "app should render ANSI output over wss://"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
