//! Attachments (#95): link a file-area file to a board post or private mail.
//!
//! Nothing is copied. An attachment is a pointer at a row in `files`, so the
//! file keeps living in its area — still listed in the TUI file browser, still
//! fetchable over SFTP, and still governed by that area's `min_read_role`.
//!
//! **Attaching never widens access.** Every read here re-checks the viewer's
//! role against the owning area, so a file from a restricted area is invisible
//! to someone who couldn't already read it — it doesn't appear as "hidden", it
//! simply isn't in the list, the same way a restricted board doesn't appear in
//! the board list. Writes check the *actor's* read access too: you can't attach
//! a file you can't see.

use sqlx::sqlite::SqlitePool;

use crate::config::Limits;
use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::services::role_rank;
use crate::util::now_unix;

/// A file attached to a post or mail, joined with the area it lives in.
///
/// Carries `min_read_role` so the caller can see *why* something was filtered,
/// and so the filtering rule lives in exactly one place ([`readable`]).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Attachment {
    pub file_id: i64,
    pub area_id: i64,
    pub area_name: String,
    pub min_read_role: String,
    pub filename: String,
    pub description: String,
    pub size: i64,
    pub storage_path: String,
}

impl Attachment {
    /// `area/filename` — how an attachment is addressed over SFTP, and how it
    /// is shown in the reader so the two obviously refer to the same thing.
    pub fn path(&self) -> String {
        format!("{}/{}", self.area_name, self.filename)
    }
}

/// Drop attachments the viewer's role can't read.
///
/// Filtered in Rust rather than SQL because role ordering is [`role_rank`],
/// not a column — the same shape as `boards::list_readable_boards` and
/// `files::list_readable_areas`.
fn readable(rows: Vec<Attachment>, role: &str) -> Vec<Attachment> {
    let rank = role_rank(role);
    rows.into_iter()
        .filter(|a| rank >= role_rank(&a.min_read_role))
        .collect()
}

// The two read queries share a projection but are spelled out in full: sqlx
// only accepts statically-known SQL, so assembling them from a shared `const`
// fragment isn't an option.

/// Attachments on a board post that `role` may read, oldest first.
pub async fn for_message(
    pool: &SqlitePool,
    message_id: i64,
    role: &str,
) -> Result<Vec<Attachment>> {
    let rows = sqlx::query_as::<_, Attachment>(
        "SELECT f.id AS file_id, f.area_id, a.name AS area_name, a.min_read_role, \
         f.filename, f.description, f.size, f.storage_path \
         FROM message_attachments ma \
         JOIN files f ON f.id = ma.file_id \
         JOIN file_areas a ON a.id = f.area_id \
         WHERE ma.message_id = ? ORDER BY ma.created_at, f.id",
    )
    .bind(message_id)
    .fetch_all(pool)
    .await?;
    Ok(readable(rows, role))
}

/// Attachments on a mail message that `role` may read, oldest first.
pub async fn for_mail(pool: &SqlitePool, mail_id: i64, role: &str) -> Result<Vec<Attachment>> {
    let rows = sqlx::query_as::<_, Attachment>(
        "SELECT f.id AS file_id, f.area_id, a.name AS area_name, a.min_read_role, \
         f.filename, f.description, f.size, f.storage_path \
         FROM mail_attachments ma \
         JOIN files f ON f.id = ma.file_id \
         JOIN file_areas a ON a.id = f.area_id \
         WHERE ma.mail_id = ? ORDER BY ma.created_at, f.id",
    )
    .bind(mail_id)
    .fetch_all(pool)
    .await?;
    Ok(readable(rows, role))
}

/// Every file `role` may read, newest first — the compose-time attach picker.
///
/// Deliberately flat (`area/filename` in one list) rather than a nested
/// area-then-file walk: a BBS holds few enough files that one scrollable list
/// is faster to use than two screens of navigation.
pub async fn pickable(pool: &SqlitePool, role: &str) -> Result<Vec<Attachment>> {
    let rows = sqlx::query_as::<_, Attachment>(
        "SELECT f.id AS file_id, f.area_id, a.name AS area_name, a.min_read_role, \
         f.filename, f.description, f.size, f.storage_path \
         FROM files f JOIN file_areas a ON a.id = f.area_id \
         ORDER BY f.id DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(readable(rows, role))
}

/// Whether `actor` may attach `file_id` — i.e. can read the area it lives in.
/// A file the actor can't see isn't theirs to point other people at.
async fn may_attach(pool: &SqlitePool, file_id: i64, actor: &User) -> Result<bool> {
    let min_read: Option<String> = sqlx::query_scalar(
        "SELECT a.min_read_role FROM files f JOIN file_areas a ON a.id = f.area_id \
         WHERE f.id = ?",
    )
    .bind(file_id)
    .fetch_optional(pool)
    .await?;
    Ok(min_read.is_some_and(|r| role_rank(&actor.role) >= role_rank(&r)))
}

/// How many attachments an item may carry, or `None` when uncapped.
fn cap(limits: &Limits) -> Option<i64> {
    (limits.max_attachments > 0).then(|| i64::from(limits.max_attachments))
}

/// Attach `file_id` to a board post the actor wrote.
///
/// Ownership is a `WHERE EXISTS` inside the INSERT rather than a separate
/// lookup, so there's no window between checking and writing. Returns
/// `AppError::NotFound` when the post isn't the actor's (or doesn't exist) —
/// deliberately the same answer either way, so this can't probe for posts.
pub async fn attach_to_message(
    pool: &SqlitePool,
    message_id: i64,
    file_id: i64,
    actor: &User,
    limits: &Limits,
) -> Result<()> {
    if actor.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    if !may_attach(pool, file_id, actor).await? {
        return Err(AppError::NotFound);
    }
    if let Some(max) = cap(limits) {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?")
                .bind(message_id)
                .fetch_one(pool)
                .await?;
        if n >= max {
            return Err(AppError::TooManyAttachments(limits.max_attachments));
        }
    }
    let affected = sqlx::query(
        "INSERT OR IGNORE INTO message_attachments (message_id, file_id, created_at) \
         SELECT ?, ?, ? WHERE EXISTS (SELECT 1 FROM messages WHERE id = ? AND author_id = ?)",
    )
    .bind(message_id)
    .bind(file_id)
    .bind(now_unix())
    .bind(message_id)
    .bind(actor.id)
    .execute(pool)
    .await?
    .rows_affected();
    // 0 rows means either "not your post" or "already attached". The re-check
    // is itself scoped to the actor's own post — otherwise someone else having
    // already attached the same file would forgive an unauthorized attach.
    if affected == 0 && !is_attached_to_message(pool, message_id, file_id, actor.id).await? {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Attach `file_id` to a mail message the actor sent. See
/// [`attach_to_message`] for the ownership and idempotency rules.
pub async fn attach_to_mail(
    pool: &SqlitePool,
    mail_id: i64,
    file_id: i64,
    actor: &User,
    limits: &Limits,
) -> Result<()> {
    if actor.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    if !may_attach(pool, file_id, actor).await? {
        return Err(AppError::NotFound);
    }
    if let Some(max) = cap(limits) {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_attachments WHERE mail_id = ?")
            .bind(mail_id)
            .fetch_one(pool)
            .await?;
        if n >= max {
            return Err(AppError::TooManyAttachments(limits.max_attachments));
        }
    }
    let affected = sqlx::query(
        "INSERT OR IGNORE INTO mail_attachments (mail_id, file_id, created_at) \
         SELECT ?, ?, ? WHERE EXISTS (SELECT 1 FROM mail WHERE id = ? AND from_id = ?)",
    )
    .bind(mail_id)
    .bind(file_id)
    .bind(now_unix())
    .bind(mail_id)
    .bind(actor.id)
    .execute(pool)
    .await?
    .rows_affected();
    if affected == 0 && !is_attached_to_mail(pool, mail_id, file_id, actor.id).await? {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Whether `file_id` is already attached to a post **authored by `actor_id`**.
/// The author scope is the point: it makes a repeat attach idempotent without
/// letting a stranger's existing attachment excuse an unauthorized one.
async fn is_attached_to_message(
    pool: &SqlitePool,
    message_id: i64,
    file_id: i64,
    actor_id: i64,
) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM message_attachments ma JOIN messages m ON m.id = ma.message_id \
         WHERE ma.message_id = ? AND ma.file_id = ? AND m.author_id = ?",
    )
    .bind(message_id)
    .bind(file_id)
    .bind(actor_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// The mail counterpart to [`is_attached_to_message`], scoped to the sender.
async fn is_attached_to_mail(
    pool: &SqlitePool,
    mail_id: i64,
    file_id: i64,
    actor_id: i64,
) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mail_attachments ma JOIN mail m ON m.id = ma.mail_id \
         WHERE ma.mail_id = ? AND ma.file_id = ? AND m.from_id = ?",
    )
    .bind(mail_id)
    .bind(file_id)
    .bind(actor_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// How many attachments each of `message_ids` carries, for the message-list
/// marker. Returns only ids with at least one, and does **not** apply the read
/// ACL — it's a count for the author's own list view, never file identities.
pub async fn message_counts(
    pool: &SqlitePool,
    board_id: i64,
) -> Result<std::collections::HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT ma.message_id, COUNT(*) FROM message_attachments ma \
         JOIN messages m ON m.id = ma.message_id \
         WHERE m.board_id = ? GROUP BY ma.message_id",
    )
    .bind(board_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}
