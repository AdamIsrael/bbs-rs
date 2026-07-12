//! Database connection and migrations.

pub mod models;

use std::collections::HashSet;

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

/// One migration and whether it has been applied to the database.
#[derive(Debug, Clone)]
pub struct MigrationStatus {
    pub version: i64,
    pub description: String,
    pub applied: bool,
}

/// Versions currently recorded in `_sqlx_migrations` (empty if the table
/// doesn't exist yet, i.e. no migration has ever run).
async fn applied_versions(pool: &SqlitePool) -> Result<HashSet<i64>> {
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table.is_none() {
        return Ok(HashSet::new());
    }
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?;
    Ok(versions.into_iter().collect())
}

/// The embedded migrations and their applied/pending state, in version order.
pub async fn migration_status(pool: &SqlitePool) -> Result<Vec<MigrationStatus>> {
    let applied = applied_versions(pool).await?;
    let migrator = sqlx::migrate!("./migrations");
    Ok(migrator
        .iter()
        .map(|m| MigrationStatus {
            version: m.version,
            description: m.description.to_string(),
            applied: applied.contains(&m.version),
        })
        .collect())
}

/// Apply pending migrations and return the versions that were newly applied
/// (empty if the database was already up to date), in ascending order.
pub async fn run_migrations_reporting(pool: &SqlitePool) -> Result<Vec<i64>> {
    let before = applied_versions(pool).await?;
    sqlx::migrate!("./migrations").run(pool).await?;
    let after = applied_versions(pool).await?;
    let mut newly: Vec<i64> = after.difference(&before).copied().collect();
    newly.sort_unstable();
    Ok(newly)
}
