//! Oneliners — a shared public "graffiti wall". Any registered user can append
//! one; guests are read-only, like boards and mail.
//!
//! These are also the BBS's **ActivityPub statuses** (#108): each oneliner is a
//! `Note` attributed to its author, so a user's oneliners are their outbox and
//! the wall is the instance's local timeline.
//!
//! That federation role is why the wall no longer auto-trims. A federated post
//! has a permanent URI; deleting one out from under remote servers would orphan
//! their references and demand `Delete` fan-out. The wall therefore grows
//! without bound, and moderation (`bbsctl rm-oneliner`) replaces the old ring
//! buffer — a deliberate reversal of #32.

use sqlx::sqlite::SqlitePool;

use crate::config::{Limits, Oneliners};
use crate::db::models::{Oneliner, User};
use crate::error::{AppError, Result};
use crate::services::enforce_rate;
use crate::util::now_unix;

/// Default maximum length of a oneliner body, in characters (the
/// `[oneliners] max_length` default). Longer (or empty) input is rejected with
/// [`AppError::OnelinerLength`].
///
/// 500 matches Mastodon: "like a federated post" means a *server-defined*
/// limit, not none. Unbounded statuses are an abuse vector, and remote servers
/// reject oversized payloads anyway. `0` still disables the cap.
pub const MAX_LEN: usize = 500;

/// Recent oneliners, newest first, up to `limit`.
pub async fn recent(pool: &SqlitePool, limit: i64) -> Result<Vec<Oneliner>> {
    let rows = sqlx::query_as::<_, Oneliner>(
        "SELECT o.id, o.author_id, u.username AS author_name, o.body, o.created_at \
         FROM oneliners o JOIN users u ON u.id = o.author_id \
         ORDER BY o.id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One oneliner by id, or `None`. Backs the `Note` endpoint (#108).
pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<Oneliner>> {
    let row = sqlx::query_as::<_, Oneliner>(
        "SELECT o.id, o.author_id, u.username AS author_name, o.body, o.created_at \
         FROM oneliners o JOIN users u ON u.id = o.author_id WHERE o.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// A user's own oneliners, newest first — their ActivityPub outbox (#108).
pub async fn by_author(pool: &SqlitePool, author_id: i64, limit: i64) -> Result<Vec<Oneliner>> {
    let rows = sqlx::query_as::<_, Oneliner>(
        "SELECT o.id, o.author_id, u.username AS author_name, o.body, o.created_at \
         FROM oneliners o JOIN users u ON u.id = o.author_id \
         WHERE o.author_id = ? ORDER BY o.id DESC LIMIT ?",
    )
    .bind(author_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many oneliners a user has posted (the outbox's `totalItems`).
pub async fn count_by_author(pool: &SqlitePool, author_id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM oneliners WHERE author_id = ?")
            .bind(author_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Number of oneliners on the wall.
pub async fn count(pool: &SqlitePool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oneliners")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Count a user's oneliners created since `since` (Unix seconds).
async fn recent_count(pool: &SqlitePool, author_id: i64, since: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM oneliners WHERE author_id = ? AND created_at >= ?",
        )
        .bind(author_id)
        .bind(since)
        .fetch_one(pool)
        .await?,
    )
}

/// Append a oneliner to the wall. Guests are rejected; the (trimmed) body must
/// be 1..=`cfg.max_length` characters (0 disables the cap); non-admins are
/// subject to the per-user oneliner rate limit.
///
/// Returns the new oneliner's id, which the caller uses to fan the status out
/// to the author's remote followers (#109).
///
/// The wall is **not** trimmed — see the module docs. With no ring buffer the
/// rate limit (`[limits] max_oneliners`) is what keeps the wall sane.
pub async fn add(
    pool: &SqlitePool,
    author: &User,
    body: &str,
    limits: &Limits,
    cfg: &Oneliners,
) -> Result<i64> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let body = body.trim();
    if body.is_empty() || (cfg.max_length > 0 && body.chars().count() > cfg.max_length) {
        return Err(AppError::OnelinerLength(cfg.max_length));
    }
    if !author.is_admin()
        && let Some(since) = limits.window_start(now_unix())
    {
        let count = recent_count(pool, author.id, since).await?;
        enforce_rate(count, limits.max_oneliners)?;
    }
    let id = sqlx::query("INSERT INTO oneliners (author_id, body, created_at) VALUES (?, ?, ?)")
        .bind(author.id)
        .bind(body)
        .bind(now_unix())
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(id)
}

/// Delete a oneliner by id (operator moderation). Returns whether a row was
/// removed.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM oneliners WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}
