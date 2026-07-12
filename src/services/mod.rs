//! Transport-agnostic domain logic: everything the TUI needs, independent of
//! how the user connected (SSH today, WebSocket in the future).

pub mod admin;
pub mod auth;
pub mod boards;
pub mod bulletins;
pub mod mail;
pub mod oneliners;
pub mod presence;

use sqlx::sqlite::SqlitePool;

use crate::error::Result;

/// Rank a role for access comparisons: `guest` < `user` < `admin`. An unknown
/// role ranks as the most restrictive (0), so a typo never grants access.
pub fn role_rank(role: &str) -> usize {
    admin::ROLES.iter().position(|r| *r == role).unwrap_or(0)
}

/// One-time startup seeding: the guest account and default boards.
pub async fn seed(pool: &SqlitePool) -> Result<()> {
    auth::ensure_guest(pool).await?;
    boards::ensure_default_boards(pool).await?;
    Ok(())
}
