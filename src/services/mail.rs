//! Simple user-to-user private mail.

use sqlx::sqlite::SqlitePool;

use crate::db::models::{Mail, User};
use crate::error::{AppError, Result};
use crate::services::auth;
use crate::util::now_unix;

/// All mail addressed to a user, newest first.
pub async fn inbox(pool: &SqlitePool, user_id: i64) -> Result<Vec<Mail>> {
    let mail = sqlx::query_as::<_, Mail>(
        "SELECT m.id, m.from_id, m.to_id, u.username AS from_name, \
         m.subject, m.body, m.created_at, m.read_at \
         FROM mail m JOIN users u ON u.id = m.from_id \
         WHERE m.to_id = ? ORDER BY m.id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(mail)
}

/// Fetch a single message (scoped to the recipient) and mark it read.
pub async fn read_mail(pool: &SqlitePool, id: i64, user_id: i64) -> Result<Mail> {
    let mut mail = sqlx::query_as::<_, Mail>(
        "SELECT m.id, m.from_id, m.to_id, u.username AS from_name, \
         m.subject, m.body, m.created_at, m.read_at \
         FROM mail m JOIN users u ON u.id = m.from_id \
         WHERE m.id = ? AND m.to_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    if mail.read_at.is_none() {
        let ts = now_unix();
        sqlx::query("UPDATE mail SET read_at = ? WHERE id = ?")
            .bind(ts)
            .bind(id)
            .execute(pool)
            .await?;
        mail.read_at = Some(ts);
    }
    Ok(mail)
}

/// Send mail to a named recipient. Guests are rejected.
pub async fn send_mail(
    pool: &SqlitePool,
    from: &User,
    to_username: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    if from.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let to = auth::find_user(pool, to_username)
        .await?
        .ok_or(AppError::RecipientNotFound)?;
    sqlx::query(
        "INSERT INTO mail (from_id, to_id, subject, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(from.id)
    .bind(to.id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .execute(pool)
    .await?;
    Ok(())
}
