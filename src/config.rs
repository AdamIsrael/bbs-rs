use std::path::PathBuf;

use clap::Parser;

/// Runtime configuration, populated from CLI flags (with sensible defaults).
#[derive(Parser, Debug, Clone)]
#[command(
    name = "sshtui",
    about = "A bare-bones bulletin board system served over SSH"
)]
pub struct Config {
    /// Address to bind the SSH server to.
    #[arg(long, default_value = "0.0.0.0")]
    pub host: String,

    /// TCP port for the SSH server.
    #[arg(long, default_value_t = 2222)]
    pub port: u16,

    /// SQLite database URL. `mode=rwc` creates the file if missing.
    #[arg(long, default_value = "sqlite://bbs.db?mode=rwc")]
    pub database_url: String,

    /// Path to the persistent SSH host key (generated on first run if absent).
    #[arg(long, default_value = "host_key")]
    pub host_key: PathBuf,
}
