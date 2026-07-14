//! Transport-agnostic domain logic: everything the TUI needs, independent of
//! how the user connected (SSH today, WebSocket in the future).

pub mod admin;
pub mod archive;
pub mod auth;
pub mod boards;
pub mod bulletins;
pub mod files;
pub mod keys;
pub mod mail;
pub mod oneliners;
pub mod presence;
pub mod profiles;
pub mod search;
pub mod stats;

use sqlx::sqlite::SqlitePool;

use crate::error::{AppError, Result};

/// Reject an action when the user has reached its per-window cap. `max == 0`
/// disables the limit. Used by the post/mail/oneliner services to throttle
/// spam; callers count the user's recent rows and pass the total as `count`.
pub fn enforce_rate(count: i64, max: u32) -> Result<()> {
    if max > 0 && count >= i64::from(max) {
        return Err(AppError::RateLimited);
    }
    Ok(())
}

/// Rank a role for access comparisons: `guest` < `user` < `admin`. An unknown
/// role ranks as the most restrictive (0), so a typo never grants access.
pub fn role_rank(role: &str) -> usize {
    admin::ROLES.iter().position(|r| *r == role).unwrap_or(0)
}

/// One-time startup seeding: the guest account, default boards, and a default
/// file area.
pub async fn seed(pool: &SqlitePool) -> Result<()> {
    auth::ensure_guest(pool).await?;
    boards::ensure_default_boards(pool).await?;
    files::ensure_default_areas(pool).await?;
    Ok(())
}
