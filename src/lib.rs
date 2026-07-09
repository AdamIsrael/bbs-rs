//! sshtui — a bare-bones bulletin board system served over SSH.
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

use config::Config;

/// Open the database, run migrations, seed defaults, and serve over SSH.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = db::connect(&config.database_url).await?;
    db::run_migrations(&pool).await?;
    services::seed(&pool).await?;

    tracing::info!(
        "sshtui BBS listening on {}:{} (try: ssh guest@{} -p {}, password 'guest')",
        config.host,
        config.port,
        config.host,
        config.port
    );

    ssh::run(&config, pool).await
}
