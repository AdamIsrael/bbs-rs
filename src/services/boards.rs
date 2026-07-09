//! Message board reads and posting.

use sqlx::sqlite::SqlitePool;

use crate::db::models::{Board, Message, User};
use crate::error::{AppError, Result};
use crate::util::now_unix;

pub async fn list_boards(pool: &SqlitePool) -> Result<Vec<Board>> {
    let boards = sqlx::query_as::<_, Board>("SELECT id, name, description FROM boards ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(boards)
}

pub async fn list_messages(pool: &SqlitePool, board_id: i64) -> Result<Vec<Message>> {
    let messages = sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.board_id = ? ORDER BY m.id DESC",
    )
    .bind(board_id)
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

pub async fn get_message(pool: &SqlitePool, id: i64) -> Result<Message> {
    sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Post a new message. Guests are rejected.
pub async fn post_message(
    pool: &SqlitePool,
    board_id: i64,
    author: &User,
    subject: &str,
    body: &str,
) -> Result<()> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    sqlx::query(
        "INSERT INTO messages (board_id, author_id, subject, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(board_id)
    .bind(author.id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .execute(pool)
    .await?;
    Ok(())
}

/// Seed a couple of default boards on first run.
pub async fn ensure_default_boards(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boards")
        .fetch_one(pool)
        .await?;
    if count == 0 {
        for (name, desc) in [
            ("General", "General chatter and introductions"),
            ("Announcements", "System news and updates"),
        ] {
            sqlx::query("INSERT INTO boards (name, description) VALUES (?, ?)")
                .bind(name)
                .bind(desc)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
