//! Per-user ignore / block list (#97).
//!
//! A user can block another local user to hide their board posts and refuse
//! their mail and pages. Blocks are one-directional (blocking someone doesn't
//! block you from them) and mutual visibility is not implied. Operators are
//! exempt: an `admin` can't be blocked, so a sysop can always reach every user.

use std::collections::HashSet;

use sqlx::sqlite::SqlitePool;

use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// Block `blocked` on behalf of `blocker`. Idempotent. Refuses self-blocks,
/// guests (the shared account has no personal list), and admins (operators must
/// stay reachable).
pub async fn block(pool: &SqlitePool, blocker: &User, blocked: &User) -> Result<()> {
    if blocker.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    if blocker.id == blocked.id {
        return Err(AppError::Blocked); // "can't block yourself" — surfaced by the UI guard too
    }
    if blocked.is_admin() {
        return Err(AppError::Blocked);
    }
    sqlx::query(
        "INSERT INTO user_blocks (blocker_id, blocked_id, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(blocker_id, blocked_id) DO NOTHING",
    )
    .bind(blocker.id)
    .bind(blocked.id)
    .bind(now_unix())
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove a block (idempotent).
pub async fn unblock(pool: &SqlitePool, blocker_id: i64, blocked_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM user_blocks WHERE blocker_id = ? AND blocked_id = ?")
        .bind(blocker_id)
        .bind(blocked_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether `blocker` has blocked `blocked`.
pub async fn is_blocked(pool: &SqlitePool, blocker_id: i64, blocked_id: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_blocks WHERE blocker_id = ? AND blocked_id = ?",
    )
    .bind(blocker_id)
    .bind(blocked_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// The set of user ids `blocker` has blocked — for filtering rendered lists
/// (board threads) in one pass. Empty for the guest account.
pub async fn blocked_ids(pool: &SqlitePool, blocker_id: i64) -> Result<HashSet<i64>> {
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT blocked_id FROM user_blocks WHERE blocker_id = ?")
            .bind(blocker_id)
            .fetch_all(pool)
            .await?;
    Ok(ids.into_iter().collect())
}

/// The users `blocker` has blocked, as `(id, username)`, alphabetical — for the
/// manage-ignored-users screen.
pub async fn list_blocked(pool: &SqlitePool, blocker_id: i64) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT u.id, u.username FROM user_blocks b JOIN users u ON u.id = b.blocked_id \
         WHERE b.blocker_id = ? ORDER BY u.username",
    )
    .bind(blocker_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
