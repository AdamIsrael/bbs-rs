use clap::Parser;
use tracing_subscriber::EnvFilter;

use bbs_rs::config::{Cli, Settings};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Log to stderr so it never corrupts a client's terminal stream. Init before
    // loading config so first-run "wrote default config" is visible.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let settings = Settings::load(&cli)?;
    if cli.migrate {
        bbs_rs::migrate(settings).await
    } else {
        bbs_rs::serve(cli, settings).await
    }
}
