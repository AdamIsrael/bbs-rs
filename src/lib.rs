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

use std::sync::Arc;

use config::Settings;

/// Open the database, run migrations, seed defaults, and serve over SSH.
pub async fn serve(settings: Settings) -> anyhow::Result<()> {
    let pool = db::connect(&settings.network.database_url).await?;
    db::run_migrations(&pool).await?;
    services::seed(&pool).await?;

    let config = Arc::new(settings);
    tracing::info!(
        "{} listening on {}:{}",
        config.bbs.name,
        config.network.host,
        config.network.port
    );

    ssh::run(config, pool).await
}
