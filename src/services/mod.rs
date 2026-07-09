//! Transport-agnostic domain logic: everything the TUI needs, independent of
//! how the user connected (SSH today, WebSocket in the future).

pub mod auth;
pub mod boards;
pub mod mail;
pub mod presence;

use sqlx::sqlite::SqlitePool;

use crate::error::Result;

/// One-time startup seeding: the guest account and default boards.
pub async fn seed(pool: &SqlitePool) -> Result<()> {
    auth::ensure_guest(pool).await?;
    boards::ensure_default_boards(pool).await?;
    Ok(())
}
