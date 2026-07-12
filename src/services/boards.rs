//! Message board reads and posting, plus per-board ACLs and moderation.
//!
//! Boards carry a read/write minimum role and a lock flag; messages carry a
//! pin flag. Read/write access is enforced here; the moderation mutators
//! (pin/lock/delete) are ungated like [`crate::services::admin`] — callers (the
//! admin-only TUI actions and `bbsctl`) gate them.

use sqlx::sqlite::SqlitePool;

use crate::config::Limits;
use crate::db::models::{Board, Message, User};
use crate::error::{AppError, Result};
use crate::services::enforce_rate;
use crate::util::now_unix;

/// All boards, in id order.
pub async fn list_boards(pool: &SqlitePool) -> Result<Vec<Board>> {
    let boards =
        sqlx::query_as::<_, Board>("SELECT id, name, description, min_read_role, min_write_role, locked FROM boards ORDER BY id")
            .fetch_all(pool)
            .await?;
    Ok(boards)
}

/// Boards a viewer with `role` is allowed to read.
pub async fn list_readable_boards(pool: &SqlitePool, role: &str) -> Result<Vec<Board>> {
    Ok(list_boards(pool)
        .await?
        .into_iter()
        .filter(|b| b.can_read(role))
        .collect())
}

/// Fetch a single board by id.
pub async fn get_board(pool: &SqlitePool, id: i64) -> Result<Board> {
    sqlx::query_as::<_, Board>(
        "SELECT id, name, description, min_read_role, min_write_role, locked FROM boards WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Messages on a board, pinned first, then newest first.
pub async fn list_messages(pool: &SqlitePool, board_id: i64) -> Result<Vec<Message>> {
    let messages = sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at, m.pinned \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.board_id = ? ORDER BY m.pinned DESC, m.id DESC",
    )
    .bind(board_id)
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

pub async fn get_message(pool: &SqlitePool, id: i64) -> Result<Message> {
    sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at, m.pinned \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Count a user's board posts created since `since` (Unix seconds).
async fn recent_post_count(pool: &SqlitePool, author_id: i64, since: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE author_id = ? AND created_at >= ?")
            .bind(author_id)
            .bind(since)
            .fetch_one(pool)
            .await?,
    )
}

/// Post a new message, enforcing the board's write ACL and lock, then the
/// per-user post rate limit. Guests are always read-only; a locked board
/// rejects non-admins (admins can still post, e.g. to add a closing note);
/// otherwise the author's role must meet the board's `min_write_role`. Admins
/// are never rate-limited.
pub async fn post_message(
    pool: &SqlitePool,
    board_id: i64,
    author: &User,
    subject: &str,
    body: &str,
    limits: &Limits,
) -> Result<()> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let board = get_board(pool, board_id).await?;
    if board.locked && !author.is_admin() {
        return Err(AppError::BoardLocked);
    }
    if !board.can_write(&author.role) {
        return Err(AppError::BoardWriteDenied);
    }
    if !author.is_admin()
        && let Some(since) = limits.window_start(now_unix())
    {
        let count = recent_post_count(pool, author.id, since).await?;
        enforce_rate(count, limits.max_posts)?;
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

// ---- Moderation (ungated; callers must be admins) -----------------------

/// Delete a message by id. Returns whether a row was removed.
pub async fn delete_message(pool: &SqlitePool, id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Pin or unpin a message.
pub async fn set_pinned(pool: &SqlitePool, id: i64, pinned: bool) -> Result<()> {
    sqlx::query("UPDATE messages SET pinned = ? WHERE id = ?")
        .bind(pinned)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Lock or unlock a board (a locked board accepts no new posts).
pub async fn set_locked(pool: &SqlitePool, board_id: i64, locked: bool) -> Result<()> {
    sqlx::query("UPDATE boards SET locked = ? WHERE id = ?")
        .bind(locked)
        .bind(board_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Set a board's read and/or write minimum role. Each is validated against the
/// known roles; `None` leaves that side unchanged.
pub async fn set_roles(
    pool: &SqlitePool,
    board_name: &str,
    min_read: Option<&str>,
    min_write: Option<&str>,
) -> Result<()> {
    for role in [min_read, min_write].into_iter().flatten() {
        if !crate::services::admin::ROLES.contains(&role) {
            return Err(AppError::BadRole(role.to_string()));
        }
    }
    if let Some(r) = min_read {
        sqlx::query("UPDATE boards SET min_read_role = ? WHERE name = ?")
            .bind(r)
            .bind(board_name)
            .execute(pool)
            .await?;
    }
    if let Some(w) = min_write {
        sqlx::query("UPDATE boards SET min_write_role = ? WHERE name = ?")
            .bind(w)
            .bind(board_name)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Lock or unlock a board by name (operator helper for `bbsctl`).
pub async fn set_locked_by_name(pool: &SqlitePool, board_name: &str, locked: bool) -> Result<()> {
    sqlx::query("UPDATE boards SET locked = ? WHERE name = ?")
        .bind(locked)
        .bind(board_name)
        .execute(pool)
        .await?;
    Ok(())
}

/// Seed a couple of default boards on first run. Announcements is admin-only to
/// post to (readable by everyone), showcasing the write ACL.
pub async fn ensure_default_boards(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boards")
        .fetch_one(pool)
        .await?;
    if count == 0 {
        for (name, desc, min_write) in [
            ("General", "General chatter and introductions", "user"),
            ("Announcements", "System news and updates", "admin"),
        ] {
            sqlx::query("INSERT INTO boards (name, description, min_write_role) VALUES (?, ?, ?)")
                .bind(name)
                .bind(desc)
                .bind(min_write)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
