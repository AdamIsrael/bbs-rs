//! Message board reads and posting, plus per-board ACLs and moderation.
//!
//! Boards carry a read/write minimum role and a lock flag; messages carry a
//! pin flag. Read/write access is enforced here; the moderation mutators
//! (pin/lock/delete) are ungated like [`crate::services::admin`] — callers (the
//! admin-only TUI actions and `bbsctl`) gate them.

use sqlx::sqlite::SqlitePool;

use crate::config::{Limits, SeedBoard};
use crate::db::models::{Board, Message, User};
use crate::error::{AppError, Result};
use crate::services::{enforce_len, enforce_rate};
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
         m.subject, m.body, m.created_at, m.pinned, m.parent_id \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.board_id = ? ORDER BY m.pinned DESC, m.id DESC",
    )
    .bind(board_id)
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// A board's root posts (`parent_id IS NULL`), newest first, up to `limit`,
/// with the total root count. Backs the Group outbox (#111): only top-level
/// posts become `Page` objects; replies are `Note`s under them.
pub async fn root_posts(
    pool: &SqlitePool,
    board_id: i64,
    limit: i64,
) -> Result<(Vec<Message>, i64)> {
    let rows = sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at, m.pinned, m.parent_id \
         FROM messages m JOIN users u ON u.id = m.author_id \
         WHERE m.board_id = ? AND m.parent_id IS NULL ORDER BY m.id DESC LIMIT ?",
    )
    .bind(board_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE board_id = ? AND parent_id IS NULL",
    )
    .bind(board_id)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// A message plus its depth in the reply tree (0 = top-level thread root).
#[derive(Debug, Clone)]
pub struct ThreadItem {
    pub message: Message,
    pub depth: u16,
}

/// A board's messages arranged as reply threads: each root (top-level post),
/// pinned first then newest, is followed depth-first by its replies (oldest
/// first). Replies whose parent is missing (e.g. deleted) become roots so
/// nothing disappears.
pub async fn list_thread(pool: &SqlitePool, board_id: i64) -> Result<Vec<ThreadItem>> {
    use std::collections::{HashMap, HashSet};

    let all = list_messages(pool, board_id).await?;
    let ids: HashSet<i64> = all.iter().map(|m| m.id).collect();

    // Group children by their effective parent (None for roots / orphans).
    let mut children: HashMap<Option<i64>, Vec<Message>> = HashMap::new();
    for m in all {
        let parent = m.parent_id.filter(|pid| ids.contains(pid));
        children.entry(parent).or_default().push(m);
    }

    // Roots keep the flat order from `list_messages` (pinned first, newest
    // first). Iterative depth-first walk; children go oldest-first.
    let mut roots = children.remove(&None).unwrap_or_default();
    let mut stack: Vec<(Message, u16)> = roots.drain(..).rev().map(|m| (m, 0)).collect();
    let mut order = Vec::new();
    while let Some((m, depth)) = stack.pop() {
        let id = m.id;
        order.push(ThreadItem { message: m, depth });
        if let Some(mut kids) = children.remove(&Some(id)) {
            kids.sort_by_key(|k| k.id);
            for k in kids.into_iter().rev() {
                stack.push((k, depth + 1));
            }
        }
    }
    Ok(order)
}

// ---- Unread tracking ("new since last call") ---------------------------

/// The Unix timestamp up to which `user_id` has seen `board_id`, or `0` if the
/// user has never opened that board.
pub async fn last_seen(pool: &SqlitePool, user_id: i64, board_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT last_seen_at FROM user_board_seen WHERE user_id = ? AND board_id = ?",
    )
    .bind(user_id)
    .bind(board_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or(0))
}

/// Record that `user_id` has seen `board_id` as of `at` (Unix seconds). The
/// watermark only moves forward, so an out-of-order call can't hide messages.
pub async fn mark_board_seen(
    pool: &SqlitePool,
    user_id: i64,
    board_id: i64,
    at: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO user_board_seen (user_id, board_id, last_seen_at) VALUES (?, ?, ?) \
         ON CONFLICT(user_id, board_id) DO UPDATE SET last_seen_at = MAX(last_seen_at, excluded.last_seen_at)",
    )
    .bind(user_id)
    .bind(board_id)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Unread message counts for `user_id`, keyed by board id. A message is unread
/// if it is newer than the user's watermark for its board and was written by
/// someone else (your own posts never count as unread). Boards with no unread
/// messages are omitted from the map.
pub async fn unread_counts(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<std::collections::HashMap<i64, i64>> {
    let rows = sqlx::query_as::<_, (i64, i64)>(
        "SELECT m.board_id, COUNT(*) \
         FROM messages m \
         LEFT JOIN user_board_seen s ON s.user_id = ? AND s.board_id = m.board_id \
         WHERE m.author_id != ? AND m.created_at > COALESCE(s.last_seen_at, 0) \
         GROUP BY m.board_id",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

pub async fn get_message(pool: &SqlitePool, id: i64) -> Result<Message> {
    sqlx::query_as::<_, Message>(
        "SELECT m.id, m.board_id, m.author_id, u.username AS author_name, \
         m.subject, m.body, m.created_at, m.pinned, m.parent_id \
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
    parent_id: Option<i64>,
    limits: &Limits,
) -> Result<i64> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    enforce_len("Subject", subject, limits.max_subject_chars)?;
    enforce_len("Message", body, limits.max_body_chars)?;
    let board = get_board(pool, board_id).await?;
    if board.locked && !author.is_admin() {
        return Err(AppError::BoardLocked);
    }
    if !board.can_write(&author.role) {
        return Err(AppError::BoardWriteDenied);
    }
    // A reply must target a message on the same board.
    if let Some(pid) = parent_id {
        let parent = get_message(pool, pid).await?;
        if parent.board_id != board_id {
            return Err(AppError::NotFound);
        }
    }
    if !author.is_admin()
        && let Some(since) = limits.window_start(now_unix())
    {
        let count = recent_post_count(pool, author.id, since).await?;
        enforce_rate(count, limits.max_posts)?;
    }
    let id = sqlx::query(
        "INSERT INTO messages (board_id, author_id, subject, body, created_at, parent_id) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(board_id)
    .bind(author.id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .bind(parent_id)
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Store a post that arrived from a remote instance into a local board (#112).
///
/// Bypasses the local ACL/rate path deliberately — a remote server enforces its
/// own, and the caller applies federation-side guards. Instead it is:
/// - **deduped** on `ap_id` (a redelivery inserts once), and
/// - **threaded**: `in_reply_to_uri` is resolved to a local parent by its
///   `ap_id`, so a remote reply nests under the post it answers (falling back to
///   a root post when we've never seen the parent).
///
/// Returns the new message id, or `None` if we already had it.
pub async fn store_remote_post(
    pool: &SqlitePool,
    board_id: i64,
    author_id: i64,
    subject: &str,
    body: &str,
    ap_id: &str,
    in_reply_to_uri: Option<&str>,
) -> Result<Option<i64>> {
    let parent_id: Option<i64> = match in_reply_to_uri {
        Some(uri) => {
            sqlx::query_scalar("SELECT id FROM messages WHERE ap_id = ?")
                .bind(uri)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };
    let affected = sqlx::query(
        "INSERT INTO messages \
           (board_id, author_id, subject, body, created_at, parent_id, ap_id, in_reply_to_uri) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(ap_id) DO NOTHING",
    )
    .bind(board_id)
    .bind(author_id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .bind(parent_id)
    .bind(ap_id)
    .bind(in_reply_to_uri)
    .execute(pool)
    .await?;
    if affected.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(affected.last_insert_rowid()))
}

/// How many posts an author has made to a board since `since` — the inbound
/// flood guard for remote authors (#112).
pub async fn author_post_count_since(pool: &SqlitePool, author_id: i64, since: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE author_id = ? AND created_at >= ?")
            .bind(author_id)
            .bind(since)
            .fetch_one(pool)
            .await?,
    )
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

/// Seed the operator-configured boards on first run (only when the board table
/// is empty). A board whose `min_read`/`min_write` isn't a known role is logged
/// and falls back to the safe default rather than failing startup.
pub async fn ensure_default_boards(pool: &SqlitePool, boards: &[SeedBoard]) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boards")
        .fetch_one(pool)
        .await?;
    if count != 0 {
        return Ok(());
    }
    for b in boards {
        let min_read = valid_role(&b.min_read, "guest", &b.name, "min_read");
        let min_write = valid_role(&b.min_write, "user", &b.name, "min_write");
        sqlx::query(
            "INSERT INTO boards (name, description, min_read_role, min_write_role) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&b.name)
        .bind(&b.description)
        .bind(min_read)
        .bind(min_write)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Return `role` if it's a known role, otherwise log and fall back to `default`.
fn valid_role<'a>(role: &'a str, default: &'a str, board: &str, field: &str) -> &'a str {
    if crate::services::admin::ROLES.contains(&role) {
        role
    } else {
        tracing::warn!("seed board {board:?}: invalid {field} {role:?}; using {default:?}");
        default
    }
}
