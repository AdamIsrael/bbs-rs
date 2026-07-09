use clap::Parser;
use tracing_subscriber::EnvFilter;

use sshtui::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();

    // Log to stderr so it never corrupts a client's terminal stream.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    sshtui::serve(config).await
}
