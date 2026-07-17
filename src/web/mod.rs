//! Browser frontend: an axum HTTP server that serves a self-contained
//! xterm.js page and a `/ws` WebSocket carrying the terminal.
//!
//! This is a second transport that reuses the entire [`crate::app`] unchanged.
//! A WebSocket connection mirrors the SSH session exactly: a [`WebTerminalHandle`]
//! is the `Write` byte-sink (ratatui's ANSI output → WS binary frames), and
//! incoming frames decode through the same [`crate::input`] parser into
//! [`Event`]s. Auth reuses [`auth::attempt_login`] (IP-ban + audit), and
//! sessions join the shared [`Presence`] so who's-online and kicks span SSH and
//! web alike.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::State;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use ratatui::Terminal;
use ratatui::TerminalOptions;
use ratatui::Viewport;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc;

use crate::app::{self, App};
use crate::config::Settings;
use crate::input;
use crate::services::presence::Presence;
use crate::services::{admin, auth};
use crate::transport::{Event, Transport};

pub mod activitypub;
mod terminal;
pub mod tls;
use terminal::WebTerminalHandle;
pub use tls::TlsSetup;

/// Shared HTTP state, cloned per request.
#[derive(Clone)]
pub struct WebState {
    pool: SqlitePool,
    /// Hot-swappable settings; each WS session snapshots the current value.
    config: Arc<ArcSwap<Settings>>,
    presence: Presence,
    /// Session-id source shared with SSH so ids never collide across transports.
    next_id: Arc<AtomicUsize>,
}

impl WebState {
    pub fn new(
        pool: SqlitePool,
        config: Arc<ArcSwap<Settings>>,
        presence: Presence,
        next_id: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            pool,
            config,
            presence,
            next_id,
        }
    }
}

/// Serve the browser frontend on a pre-bound listener until the process stops.
/// The caller binds (so a port conflict fails startup eagerly) and constructs
/// the [`WebState`].
pub async fn serve(listener: tokio::net::TcpListener, state: WebState) -> anyhow::Result<()> {
    let app = router(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Serve the frontend over TLS (HTTPS/WSS) on an already-bound listener. The
/// `SocketAddr` connect-info is preserved so the `/ws` handler still sees the
/// real peer IP for ban/audit.
pub async fn serve_tls(
    listener: std::net::TcpListener,
    state: WebState,
    tls: TlsSetup,
) -> anyhow::Result<()> {
    let make = router(state).into_make_service_with_connect_info::<SocketAddr>();
    match tls {
        TlsSetup::Rustls(config) => {
            axum_server::from_tcp_rustls(listener, config)?
                .serve(make)
                .await?;
        }
        TlsSetup::Acme { acceptor, driver } => {
            tokio::spawn(driver);
            axum_server::from_tcp(listener)?
                .acceptor(acceptor)
                .serve(make)
                .await?;
        }
    }
    Ok(())
}

/// Build the axum router for the browser frontend.
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route(
            "/xterm.js",
            get(|| asset(ASSET_XTERM_JS, "text/javascript")),
        )
        .route("/xterm.css", get(|| asset(ASSET_XTERM_CSS, "text/css")))
        .route(
            "/addon-fit.js",
            get(|| asset(ASSET_ADDON_FIT, "text/javascript")),
        )
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_handler))
        // ActivityPub (#107). Both 404 unless [federation] is enabled with a
        // validated origin, so a non-federating board looks like one.
        .route("/.well-known/webfinger", get(activitypub::webfinger))
        .route("/u/{username}", get(activitypub::person))
        .layer(axum::middleware::from_fn(log_request))
        .with_state(state)
}

/// Body of the health endpoint — a marker the startup self-check looks for to
/// confirm that *bbs-rs* (not another process that also bound the port) is the
/// one answering on the web address.
const HEALTH_MARKER: &str = "bbs-rs-web-ok";

async fn healthz() -> &'static str {
    HEALTH_MARKER
}

/// Probe our own web port shortly after startup and warn if the responder
/// isn't us. This catches the case a bind check can't: on macOS/BSD a wildcard
/// bind (`0.0.0.0:PORT`) succeeds even when another process holds
/// `127.0.0.1:PORT`, and local clients then reach that other process. Best
/// effort — a failed probe only warns, never stops the server.
pub async fn self_check(host: String, port: u16, tls: bool) {
    // Give the accept loop a moment to come up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A client can't connect to a wildcard bind address; probe loopback instead.
    let target = match host.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".to_string(),
        h => h.to_string(),
    };
    // Over HTTPS a cleartext probe can't complete the handshake, so use a
    // TLS client that trusts any cert (loopback only) to read /healthz.
    let probe = if tls {
        tls::probe_health_tls(&target, port).await
    } else {
        probe_health(&target, port).await
    };
    match probe {
        Ok(body) if body.contains(HEALTH_MARKER) => {
            tracing::debug!("web self-check ok on {target}:{port}");
        }
        Ok(_) => tracing::warn!(
            "web self-check: another process is answering on {target}:{port} — browsers will \
             reach it, not bbs-rs. Free the port or set a different [web] port."
        ),
        Err(e) => tracing::warn!(
            "web self-check could not reach {target}:{port} ({e}); the frontend may be unreachable."
        ),
    }
}

/// Fetch `/healthz` over a raw HTTP/1.0 request (no HTTP-client dependency).
async fn probe_health(host: &str, port: u16) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await??;
    let req = format!("GET /healthz HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut buf)).await??;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Log every HTTP request and its response status. Errors (e.g. a 404) log at
/// `info` so they're visible with the default `RUST_LOG`; successes log at
/// `debug` (`RUST_LOG=bbs_rs=debug`).
async fn log_request(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let res = next.run(req).await;
    let status = res.status();
    if status.is_client_error() || status.is_server_error() {
        tracing::info!("web {method} {path} -> {status}");
    } else {
        tracing::debug!("web {method} {path} -> {status}");
    }
    res
}

const INDEX_HTML: &str = include_str!("static/index.html");
const ASSET_XTERM_JS: &str = include_str!("static/xterm.js");
const ASSET_XTERM_CSS: &str = include_str!("static/xterm.css");
const ASSET_ADDON_FIT: &str = include_str!("static/addon-fit.js");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn asset(body: &'static str, content_type: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, content_type)], body)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<WebState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, peer))
}

/// The client's first frame: a login handshake.
#[derive(serde::Deserialize)]
struct Login {
    user: String,
    pass: String,
}

/// Subsequent control frames (currently just terminal resize).
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    Resize { cols: u16, rows: u16 },
}

async fn handle_socket(socket: WebSocket, state: WebState, peer: SocketAddr) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let ip = peer.ip().to_string();
    // Snapshot the current settings for this session's lifetime (picks up any
    // hot-reloaded config).
    let config = state.config.load_full();

    // 1. Expect a login handshake as the first (text) frame.
    let login: Login = match ws_stream.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
            Ok(l) => l,
            Err(_) => return,
        },
        _ => return,
    };

    // 2. Authenticate — same path as SSH: guest toggle, then attempt_login
    //    (which enforces IP/account bans and records the audit trail).
    if login.user == "guest" && !config.features.guest {
        let _ = admin::record_login(&state.pool, &login.user, Some(&ip), false).await;
        let _ = ws_sink
            .send(Message::Text("\r\nGuest login is disabled.\r\n".into()))
            .await;
        return;
    }
    let user = match auth::attempt_login(&state.pool, &login.user, &login.pass, Some(&ip)).await {
        Ok(Some(u)) => u,
        _ => {
            let _ = ws_sink
                .send(Message::Text("\r\nLogin failed.\r\n".into()))
                .await;
            return;
        }
    };

    // 3. Output path: ANSI bytes from ratatui → WS binary frames.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let out_task = tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if ws_sink.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    // 4. Build the app on the shared session id and join presence.
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let (ev_tx, ev_rx) = mpsc::channel::<Event>(64);
    state
        .presence
        .join(id, user.username.clone(), Some(ip), ev_tx.clone())
        .await;

    let (cols, rows) = (config.network.default_cols, config.network.default_rows);
    // A clone of the output channel bridges door output straight to the client.
    let raw_out = out_tx.clone();
    let backend = CrosstermBackend::new(WebTerminalHandle::new(out_tx));
    let terminal = match Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)),
        },
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("web session {id}: terminal init failed: {e}");
            state.presence.leave(id).await;
            return;
        }
    };
    let app = App::new(
        state.pool.clone(),
        state.presence.clone(),
        config.clone(),
        user,
        id,
        Transport::Web,
    );
    let mut app_task = tokio::spawn(app::run(app, terminal, ev_rx, raw_out));

    // 5. Input path: decode WS frames into events until the app ends (user
    //    quit) or the client disconnects.
    let mut buf: Vec<u8> = Vec::new();
    let input = async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                Message::Binary(b) => {
                    buf.extend_from_slice(&b);
                    for event in input::drain(&mut buf) {
                        if ev_tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }
                Message::Text(t) => {
                    if let Ok(Control::Resize { cols, rows }) = serde_json::from_str(&t) {
                        let (w, h) = clamp_size(cols, rows, &config);
                        if ev_tx.send(Event::Resize(w, h)).await.is_err() {
                            return;
                        }
                    }
                }
                Message::Close(_) => return,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = &mut app_task => {
            // User quit; the app loop already left presence. Stop reading input.
        }
        _ = input => {
            // Client disconnected; end the app loop and clean up presence.
            app_task.abort();
            state.presence.leave(id).await;
        }
    }
    out_task.abort();
}

/// Clamp a browser-reported terminal size to the configured fallback when it
/// reports 0, and to a sane ceiling. Mirrors the SSH `clamp_size`.
fn clamp_size(cols: u16, rows: u16, config: &Settings) -> (u16, u16) {
    let net = &config.network;
    let w = if cols == 0 { net.default_cols } else { cols };
    let h = if rows == 0 { net.default_rows } else { rows };
    (w.min(500), h.min(300))
}
