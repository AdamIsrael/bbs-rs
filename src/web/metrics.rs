//! A Prometheus-style `/metrics` endpoint (#98).
//!
//! Served on its **own listener** (`[metrics]`), separate from the browser
//! frontend, and bound to loopback by default. That separation is the security
//! design, not an accident: gating metrics by peer address on the shared web
//! port would break silently behind a same-host reverse proxy, where every
//! request arrives from 127.0.0.1 and a loopback check stops checking anything.
//! A distinct port simply isn't proxied. It also means metrics keep working
//! with `[web]` switched off.
//!
//! The exposition format is hand-rolled — it's a few dozen lines of text and
//! not worth a dependency. Everything is a cheap `COUNT(*)` or a read of the
//! in-memory [`Presence`] registry, so a scrape every few seconds is fine.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sqlx::sqlite::SqlitePool;

use crate::config::Settings;
use crate::services::presence::Presence;

/// Prometheus' text exposition content type (version 0.0.4).
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Clone)]
pub struct MetricsState {
    pool: SqlitePool,
    presence: Presence,
    config: Arc<ArcSwap<Settings>>,
}

impl MetricsState {
    pub fn new(pool: SqlitePool, presence: Presence, config: Arc<ArcSwap<Settings>>) -> Self {
        Self {
            pool,
            presence,
            config,
        }
    }
}

pub fn router(state: MetricsState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        // A liveness probe, so a scrape config can tell "process is down" from
        // "metrics are broken".
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

/// Serve the metrics endpoint until the listener dies.
pub async fn serve(listener: tokio::net::TcpListener, state: MetricsState) -> anyhow::Result<()> {
    axum::serve(listener, router(state)).await?;
    Ok(())
}

/// One metric: `# HELP`, `# TYPE`, then the sample. Written out longhand
/// because a scraper rejects a family whose type it can't see.
fn metric(out: &mut String, name: &str, help: &str, kind: &str, value: i64) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} {kind}\n"));
    out.push_str(&format!("{name} {value}\n"));
}

/// `SELECT COUNT(*)`-shaped queries, run one at a time. A failure yields 0
/// rather than failing the whole scrape: a partial dashboard beats none, and
/// the error is logged.
async fn count(pool: &SqlitePool, what: &'static str, sql: &'static str) -> i64 {
    match sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("metrics: counting {what}: {e}");
            0
        }
    }
}

async fn metrics(State(state): State<MetricsState>) -> Response {
    let config = state.config.load();
    // The toggle is re-read per request, so turning metrics off in bbs.toml
    // takes effect on reload without a restart — the listener stays bound (it's
    // startup-bound like the other ports) but stops answering.
    if !config.metrics.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    let pool = &state.pool;
    let now = crate::util::now_unix();
    let day_ago = now - 86_400;

    let online = state.presence.list().await.len() as i64;
    let in_chat = state.presence.chat_roster().await.len() as i64;

    let mut out = String::new();
    metric(
        &mut out,
        "bbs_sessions_online",
        "Sessions currently connected, across SSH and the web frontend.",
        "gauge",
        online,
    );
    metric(
        &mut out,
        "bbs_chat_participants",
        "Sessions currently in the live chat room.",
        "gauge",
        in_chat,
    );
    metric(
        &mut out,
        "bbs_users_total",
        "Local registered accounts (discovered remote actors excluded).",
        "gauge",
        count(
            pool,
            "users",
            "SELECT COUNT(*) FROM users WHERE is_remote = 0",
        )
        .await,
    );
    metric(
        &mut out,
        "bbs_users_pending",
        "Local accounts awaiting sysop approval.",
        "gauge",
        count(
            pool,
            "pending users",
            "SELECT COUNT(*) FROM users WHERE is_remote = 0 AND validated_at IS NULL",
        )
        .await,
    );
    metric(
        &mut out,
        "bbs_users_banned",
        "Local accounts currently banned.",
        "gauge",
        count(
            pool,
            "banned users",
            "SELECT COUNT(*) FROM users WHERE is_remote = 0 AND banned_at IS NOT NULL",
        )
        .await,
    );
    metric(
        &mut out,
        "bbs_ip_bans",
        "IP bans currently in force (expired ones excluded).",
        "gauge",
        ip_bans(pool, now).await,
    );
    metric(
        &mut out,
        "bbs_posts_total",
        "Board messages stored, replies included.",
        "counter",
        count(pool, "posts", "SELECT COUNT(*) FROM messages").await,
    );
    metric(
        &mut out,
        "bbs_mail_total",
        "Private mail messages stored.",
        "counter",
        count(pool, "mail", "SELECT COUNT(*) FROM mail").await,
    );
    metric(
        &mut out,
        "bbs_files_total",
        "Files catalogued across all file areas.",
        "gauge",
        count(pool, "files", "SELECT COUNT(*) FROM files").await,
    );
    metric(
        &mut out,
        "bbs_logins_total",
        "Successful logins ever recorded.",
        "counter",
        count(
            pool,
            "logins",
            "SELECT COUNT(*) FROM logins WHERE success = 1",
        )
        .await,
    );
    metric(
        &mut out,
        "bbs_login_failures_total",
        "Failed login attempts ever recorded.",
        "counter",
        count(
            pool,
            "login failures",
            "SELECT COUNT(*) FROM logins WHERE success = 0",
        )
        .await,
    );
    // Windowed counters are the ones an alert actually fires on — "logins are
    // failing right now" isn't visible in an all-time total.
    metric(
        &mut out,
        "bbs_login_failures_24h",
        "Failed login attempts in the last 24 hours.",
        "gauge",
        recent(pool, "recent login failures", day_ago).await,
    );
    metric(
        &mut out,
        "bbs_build_info",
        "Always 1; the version is on the label.",
        "gauge",
        1,
    );
    // A labelled sample can't go through `metric`, which writes bare values.
    out.push_str(&format!(
        "bbs_build_version{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    ([(header::CONTENT_TYPE, CONTENT_TYPE)], out).into_response()
}

/// Active IP bans. Separate from [`count`] because it binds a parameter.
async fn ip_bans(pool: &SqlitePool, now: i64) -> i64 {
    let q = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM ip_bans WHERE expires_at IS NULL OR expires_at > ?",
    )
    .bind(now);
    match q.fetch_one(pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("metrics: counting ip bans: {e}");
            0
        }
    }
}

/// Failed logins since `since`.
async fn recent(pool: &SqlitePool, what: &'static str, since: i64) -> i64 {
    let q = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM logins WHERE success = 0 AND created_at >= ?",
    )
    .bind(since);
    match q.fetch_one(pool).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("metrics: counting {what}: {e}");
            0
        }
    }
}
