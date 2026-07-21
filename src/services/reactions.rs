//! Post reactions / upvotes (#94). A user can hold at most one of each
//! [`PALETTE`] reaction kind on a board post; the counts surface under the post
//! as a lightweight engagement signal.
//!
//! Reactions are local-only — they are not federated, and only apply to local
//! `messages` rows (mirrored remote posts live elsewhere). Guests, who share a
//! single account, cannot react.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use crate::config::Limits;
use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::services::enforce_rate;

/// One reaction kind: a stable `kind` id (stored, never shown) and the `glyph`
/// shown under a post.
pub struct Reaction {
    pub kind: &'static str,
    pub glyph: &'static str,
}

/// The fixed reaction palette. Order is display/hotkey order (entry `n` is
/// toggled by pressing the digit `n+1`). Positive-only by design — a downvote
/// is a different feature with different moderation implications.
pub const PALETTE: &[Reaction] = &[
    Reaction {
        kind: "up",
        glyph: "👍",
    },
    Reaction {
        kind: "love",
        glyph: "❤",
    },
    Reaction {
        kind: "laugh",
        glyph: "😄",
    },
];

/// Whether `kind` is a known palette reaction (guards untrusted input).
pub fn is_valid_kind(kind: &str) -> bool {
    PALETTE.iter().any(|r| r.kind == kind)
}

/// Toggle `user`'s `kind` reaction on a message: add it if absent (returns
/// `true`), remove it if present (returns `false`). Guests can't react; the add
/// path is rate-limited per the `[limits]` window (admins exempt), and un-
/// reacting is always allowed so a user can undo past the cap. Rejects an
/// unknown `kind` so only palette reactions are ever stored.
pub async fn toggle(
    pool: &SqlitePool,
    message_id: i64,
    user: &User,
    kind: &str,
    limits: &Limits,
    now: i64,
) -> Result<bool> {
    if user.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    if !is_valid_kind(kind) {
        return Err(AppError::NotFound);
    }
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM message_reactions WHERE message_id = ? AND user_id = ? AND kind = ?",
    )
    .bind(message_id)
    .bind(user.id)
    .bind(kind)
    .fetch_optional(pool)
    .await?;

    if existing.is_some() {
        sqlx::query(
            "DELETE FROM message_reactions WHERE message_id = ? AND user_id = ? AND kind = ?",
        )
        .bind(message_id)
        .bind(user.id)
        .bind(kind)
        .execute(pool)
        .await?;
        return Ok(false);
    }

    // Adding a reaction — throttle like posts/mail (admins exempt).
    if !user.is_admin()
        && let Some(since) = limits.window_start(now)
    {
        let count = recent_count(pool, user.id, since).await?;
        enforce_rate(count, limits.max_reactions)?;
    }
    sqlx::query(
        "INSERT INTO message_reactions (message_id, user_id, kind, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(message_id)
    .bind(user.id)
    .bind(kind)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(true)
}

/// How many reactions `user_id` has added since `since` (for rate limiting).
async fn recent_count(pool: &SqlitePool, user_id: i64, since: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM message_reactions WHERE user_id = ? AND created_at >= ?",
    )
    .bind(user_id)
    .bind(since)
    .fetch_one(pool)
    .await?)
}

/// Per-kind reaction counts for a message, in [`PALETTE`] order (kinds with no
/// reactions are included with a count of 0, so the palette renders stably).
pub async fn counts(pool: &SqlitePool, message_id: i64) -> Result<Vec<(&'static str, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, COUNT(*) FROM message_reactions WHERE message_id = ? GROUP BY kind",
    )
    .bind(message_id)
    .fetch_all(pool)
    .await?;
    let by_kind: HashMap<String, i64> = rows.into_iter().collect();
    Ok(PALETTE
        .iter()
        .map(|r| (r.kind, by_kind.get(r.kind).copied().unwrap_or(0)))
        .collect())
}

/// The set of kinds `user_id` has reacted with on a message (to highlight the
/// user's own selections).
pub async fn my_kinds(pool: &SqlitePool, message_id: i64, user_id: i64) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT kind FROM message_reactions WHERE message_id = ? AND user_id = ?")
            .bind(message_id)
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(k,)| k).collect())
}

/// Total reaction count per message for a whole board — one query for a thread
/// list's badges. Messages with no reactions are simply absent from the map;
/// scoping by board (rather than an id list) keeps the SQL static.
pub async fn totals_for_board(pool: &SqlitePool, board_id: i64) -> Result<HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT r.message_id, COUNT(*) FROM message_reactions r \
         JOIN messages m ON m.id = r.message_id \
         WHERE m.board_id = ? GROUP BY r.message_id",
    )
    .bind(board_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Total reaction count on a single message (for an incremental badge update).
pub async fn total_for_message(pool: &SqlitePool, message_id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM message_reactions WHERE message_id = ?")
            .bind(message_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Drop every reaction on a message (called when the post is deleted, since the
/// connection doesn't enforce the FK cascade).
pub async fn clear_for_message(pool: &SqlitePool, message_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM message_reactions WHERE message_id = ?")
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}
