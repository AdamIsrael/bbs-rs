//! Sysop bulletins — short dated announcements shown after login. Authored by
//! operators (via `bbsctl`); read-only for connected users.

use sqlx::sqlite::SqlitePool;

use crate::db::models::Bulletin;
use crate::error::Result;
use crate::util::now_unix;

/// All bulletins, newest first.
pub async fn list(pool: &SqlitePool) -> Result<Vec<Bulletin>> {
    let bulletins = sqlx::query_as::<_, Bulletin>(
        "SELECT id, title, body, created_at FROM bulletins ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(bulletins)
}

/// Number of bulletins (used to decide whether to show them after login).
pub async fn count(pool: &SqlitePool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bulletins")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Post a new bulletin. Returns its id.
pub async fn add(pool: &SqlitePool, title: &str, body: &str) -> Result<i64> {
    let id = sqlx::query("INSERT INTO bulletins (title, body, created_at) VALUES (?, ?, ?)")
        .bind(title)
        .bind(body)
        .bind(now_unix())
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(id)
}

/// Delete a bulletin by id. Returns whether a row was removed.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM bulletins WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}
