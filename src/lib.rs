//! bbs-rs — a bare-bones bulletin board system served over SSH.
//!
//! The crate is split so the TUI is transport-agnostic:
//! - [`services`] + [`db`] hold all domain logic (no knowledge of SSH),
//! - [`app`] is the ratatui state machine and event loop,
//! - [`transport`] + [`input`] define the byte-sink / event contract, and
//! - [`ssh`] adapts russh to that contract. A future `web` module can adapt a
//!   WebSocket + xterm.js frontend the same way, reusing everything else.

pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod input;
pub mod reload;
pub mod services;
pub mod ssh;
pub mod transport;
pub mod util;
pub mod web;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::Context;
use arc_swap::ArcSwap;

use config::{Cli, Settings};
use services::presence::Presence;

/// Open the database, run migrations, seed defaults, and serve over SSH (and,
/// when enabled, the browser frontend). Both transports share one presence
/// registry and session-id counter, so who's-online and kicks span them.
pub async fn serve(cli: Cli, settings: Settings) -> anyhow::Result<()> {
    // Install a process-wide rustls crypto provider for the web TLS stack
    // (idempotent — ignore the error if one is already installed).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let pool = db::connect(&settings.network.database_url).await?;
    db::run_migrations(&pool).await?;
    services::seed(&pool, &settings.seed).await?;

    // Settings live in an `ArcSwap` so the config file can be hot-reloaded:
    // new sessions snapshot the current value; the reload task swaps it in.
    let config = Arc::new(ArcSwap::from_pointee(settings));
    reload::spawn(cli, config.clone());

    let presence = Presence::new();
    let next_id = Arc::new(AtomicUsize::new(0));

    // A snapshot for the startup-bound values (listeners, host key). These are
    // fixed for the process lifetime — reload flags changes to them.
    let boot = config.load_full();
    if boot.web.enabled {
        // Bind the web port eagerly so a conflict (port already in use) fails
        // startup with a clear error, rather than being lost in a background
        // task while the process keeps running SSH-only. A std listener lets the
        // TLS path hand it to axum-server; the plain path converts it to tokio.
        let addr = format!("{}:{}", boot.web.host, boot.web.port);
        let std_listener = std::net::TcpListener::bind(&addr).with_context(|| {
            format!("binding web frontend to {addr} — is the port already in use?")
        })?;
        std_listener
            .set_nonblocking(true)
            .context("making the web listener non-blocking")?;
        let scheme = if boot.web.tls { "https" } else { "http" };
        tracing::info!("web frontend listening on {scheme}://{addr}");
        let mut state = web::WebState::new(
            pool.clone(),
            config.clone(),
            presence.clone(),
            next_id.clone(),
        );
        // Wire up inbound federation when it's enabled and the origin validates.
        // The origin is validated fail-closed here (same as at startup): a
        // board that can't federate correctly refuses to, rather than serving
        // an inbox that would mint permanent garbage.
        if boot.federation.enabled {
            let origin = services::federation::Origin::from_config(&boot.federation)
                .context("[federation] enabled but the origin is invalid")?;
            // Assign each board a Group slug + keypair so `/c/{slug}` and its
            // WebFinger handle are discoverable (a Group's slug is derived, not a
            // natural key like a username).
            services::federation::ensure_all_group_keys(&pool, &origin)
                .await
                .context("minting board Group identities")?;
            let fed = web::ap_object::build_config(pool.clone(), origin, &boot.federation)
                .await
                .context("building the federation config")?;
            // Drain the durable delivery queue in the background: sign each
            // outbound activity with its actor's key and POST it. The config is
            // cheap to clone (it's Arc-backed); one copy drives the drain, the
            // other powers the inbox.
            let interval = boot.federation.delivery_interval();
            let max_attempts = boot.federation.delivery_max_attempts;
            let drain_cfg = fed.clone();
            tokio::spawn(web::ap_object::run_delivery_queue(
                drain_cfg,
                interval,
                max_attempts,
            ));
            state = state.with_federation(fed);
            tracing::info!("ActivityPub federation enabled");
        }
        if boot.web.tls {
            // Resolve TLS on the main task so cert errors fail startup.
            let tls = web::tls::resolve(&boot.web)
                .await
                .context("setting up web TLS")?;
            tokio::spawn(async move {
                if let Err(e) = web::serve_tls(std_listener, state, tls).await {
                    tracing::error!("web frontend stopped: {e}");
                }
            });
        } else {
            let listener =
                tokio::net::TcpListener::from_std(std_listener).context("web listener")?;
            tokio::spawn(async move {
                if let Err(e) = web::serve(listener, state).await {
                    tracing::error!("web frontend stopped: {e}");
                }
            });
        }
        // Best-effort: warn if something other than us answers on the port.
        tokio::spawn(web::self_check(
            boot.web.host.clone(),
            boot.web.port,
            boot.web.tls,
        ));
    }

    tracing::info!(
        "{} listening on {}:{}",
        boot.bbs.name,
        boot.network.host,
        boot.network.port
    );

    ssh::run(config, pool, presence, next_id).await
}

/// Apply pending migrations and report, without starting the server. Backs
/// `bbs-rs --migrate` so a released binary can migrate after an upgrade.
pub async fn migrate(settings: Settings) -> anyhow::Result<()> {
    let pool = db::connect(&settings.network.database_url).await?;
    let newly = db::run_migrations_reporting(&pool).await?;
    if newly.is_empty() {
        println!("database is up to date");
    } else {
        for v in &newly {
            println!("applied migration {v}");
        }
        println!("applied {} migration(s)", newly.len());
    }
    Ok(())
}
