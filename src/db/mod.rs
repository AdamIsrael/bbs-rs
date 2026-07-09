//! Database connection and migrations.

pub mod models;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

use crate::error::Result;

/// Open (and, via the URL's `mode=rwc`, create) the SQLite database.
pub async fn connect(url: &str) -> Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await?;
    Ok(pool)
}

/// Apply any pending migrations from the `migrations/` directory.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
