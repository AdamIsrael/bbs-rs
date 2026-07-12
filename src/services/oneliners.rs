//! Oneliners — a shared public "graffiti wall" of short one-line posts. Any
//! registered user can append one; guests are read-only, like boards and mail.

use sqlx::sqlite::SqlitePool;

use crate::db::models::{Oneliner, User};
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// Maximum length of a oneliner body, in characters. Longer (or empty) input
/// is rejected with [`AppError::OnelinerLength`].
pub const MAX_LEN: usize = 120;

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

/// Number of oneliners on the wall.
pub async fn count(pool: &SqlitePool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM oneliners")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Append a oneliner to the wall. Guests are rejected; the (trimmed) body must
/// be 1..=[`MAX_LEN`] characters.
pub async fn add(pool: &SqlitePool, author: &User, body: &str) -> Result<()> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let body = body.trim();
    if body.is_empty() || body.chars().count() > MAX_LEN {
        return Err(AppError::OnelinerLength(MAX_LEN));
    }
    sqlx::query("INSERT INTO oneliners (author_id, body, created_at) VALUES (?, ?, ?)")
        .bind(author.id)
        .bind(body)
        .bind(now_unix())
        .execute(pool)
        .await?;
    Ok(())
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
