//! Per-user daily time limits (#75).
//!
//! Finished sessions add their duration to a per-user, per-day total; the ban
//! sweeper adds the *live* elapsed time of each connected session on top, so a
//! user can't dodge the cap by simply staying connected. Admins are exempt.
//!
//! The day key is the UTC day number (`unix_seconds / 86400`), so the budget
//! rolls over at 00:00 UTC regardless of where the caller is.

use sqlx::SqlitePool;

use crate::error::Result;

/// Seconds in a day — also the divisor that turns a timestamp into a day key.
pub const DAY_SECS: i64 = 86_400;

/// How much of the budget remains when the user is warned, in seconds.
pub const WARN_SECS: i64 = 5 * 60;

/// The UTC day number a timestamp falls in.
pub fn day_key(now: i64) -> i64 {
    now.div_euclid(DAY_SECS)
}

/// Seconds this user has already banked for `day` (finished sessions only).
pub async fn seconds_used(pool: &SqlitePool, user_id: i64, day: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT seconds FROM user_time WHERE user_id = ? AND day = ?")
            .bind(user_id)
            .bind(day)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0),
    )
}

/// Bank `secs` of connected time against `day` (upsert). Negative or zero
/// durations are ignored, so a clock skew can't credit a user time back.
pub async fn add_seconds(pool: &SqlitePool, user_id: i64, day: i64, secs: i64) -> Result<()> {
    if secs <= 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO user_time (user_id, day, seconds) VALUES (?, ?, ?) \
         ON CONFLICT(user_id, day) DO UPDATE SET seconds = seconds + excluded.seconds",
    )
    .bind(user_id)
    .bind(day)
    .bind(secs)
    .execute(pool)
    .await?;
    Ok(())
}

/// Purge usage rows older than `keep_days` (housekeeping; the table would
/// otherwise grow one row per user per day forever).
pub async fn purge_before(pool: &SqlitePool, oldest_day: i64) -> Result<u64> {
    let removed = sqlx::query("DELETE FROM user_time WHERE day < ?")
        .bind(oldest_day)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed)
}
