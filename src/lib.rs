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
pub mod services;
pub mod ssh;
pub mod transport;
pub mod util;
pub mod web;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use config::Settings;
use services::presence::Presence;

/// Open the database, run migrations, seed defaults, and serve over SSH (and,
/// when enabled, the browser frontend). Both transports share one presence
/// registry and session-id counter, so who's-online and kicks span them.
pub async fn serve(settings: Settings) -> anyhow::Result<()> {
    let pool = db::connect(&settings.network.database_url).await?;
    db::run_migrations(&pool).await?;
    services::seed(&pool).await?;

    let config = Arc::new(settings);
    let presence = Presence::new();
    let next_id = Arc::new(AtomicUsize::new(0));

    if config.web.enabled {
        let (c, p, pr, id) = (
            config.clone(),
            pool.clone(),
            presence.clone(),
            next_id.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = web::run(c, p, pr, id).await {
                tracing::error!("web frontend stopped: {e}");
            }
        });
    }

    tracing::info!(
        "{} listening on {}:{}",
        config.bbs.name,
        config.network.host,
        config.network.port
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
