//! Simple user-to-user private mail.

use sqlx::sqlite::SqlitePool;

use crate::config::Limits;
use crate::db::models::{Mail, User};
use crate::error::{AppError, Result};
use crate::services::{auth, enforce_len, enforce_rate};
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

/// A reply's prefilled `(to, subject, body)` (#70).
///
/// Bottom-posting: the original is quoted with `> ` under an attribution line,
/// and the body ends with a blank line so the compose editor's cursor (which
/// lands at the end) sits ready for the reply. `to` is the original sender's
/// name, which is what the reader would type anyway.
pub fn reply_prefill(mail: &Mail) -> (String, String, String) {
    let subject = crate::util::reply_subject(&mail.subject);
    let quoted = mail
        .body
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let attribution = format!(
        "On {}, {} wrote:",
        crate::util::fmt_time(mail.created_at),
        mail.from_name
    );
    // Trailing blank line → cursor lands there for the reply.
    let body = format!("{attribution}\n{quoted}\n\n");
    (mail.from_name.clone(), subject, body)
}

/// A forward's prefilled `(subject, body)` (#70). `to` is left for the sender to
/// choose. The original is reproduced verbatim under a header rather than quoted
/// — a forward passes the message on, it doesn't respond to it.
pub fn forward_prefill(mail: &Mail) -> (String, String) {
    let subject = if mail.subject.to_ascii_lowercase().starts_with("fwd:") {
        mail.subject.clone()
    } else {
        format!("Fwd: {}", mail.subject)
    };
    let body = format!(
        "---------- Forwarded message ----------\nFrom: {}\nSubject: {}\n\n{}",
        mail.from_name, mail.subject, mail.body
    );
    (subject, body)
}

/// Delete a message from a user's mailbox (#70).
///
/// **Scoped to the recipient in the SQL**: the `to_id = ?` clause means a user
/// can only delete mail addressed to them, so there's no separate ownership
/// check to get wrong. A mail row is a single delivery to one recipient — there
/// is no sender "sent" copy in this schema — so removing the row is exactly
/// "the recipient discards their message", nothing more. Returns whether a row
/// was removed (a wrong or already-deleted id is a no-op, not an error).
pub async fn delete_mail(pool: &SqlitePool, id: i64, user_id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM mail WHERE id = ? AND to_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Number of unread messages addressed to a user (`read_at IS NULL`). Used to
/// surface a "you have N new messages" notice at login and a main-menu badge.
pub async fn unread_count(pool: &SqlitePool, user_id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM mail WHERE to_id = ? AND read_at IS NULL")
            .bind(user_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Count mail a user has sent since `since` (Unix seconds).
async fn recent_sent_count(pool: &SqlitePool, from_id: i64, since: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM mail WHERE from_id = ? AND created_at >= ?")
            .bind(from_id)
            .bind(since)
            .fetch_one(pool)
            .await?,
    )
}

/// Shared sender-side gate: guest rejection, length caps, and the per-user rate
/// limit. Both local and remote sends run this before touching the table.
async fn check_sender(
    pool: &SqlitePool,
    from: &User,
    subject: &str,
    body: &str,
    limits: &Limits,
) -> Result<()> {
    if from.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    enforce_len("Subject", subject, limits.max_subject_chars)?;
    enforce_len("Message", body, limits.max_body_chars)?;
    if !from.is_admin()
        && let Some(since) = limits.window_start(now_unix())
    {
        let count = recent_sent_count(pool, from.id, since).await?;
        enforce_rate(count, limits.max_mail)?;
    }
    Ok(())
}

/// Insert a mail row and return its id.
async fn insert(
    pool: &SqlitePool,
    from_id: i64,
    to_id: i64,
    subject: &str,
    body: &str,
) -> Result<i64> {
    let id = sqlx::query(
        "INSERT INTO mail (from_id, to_id, subject, body, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Send mail to a named **local** recipient. Guests are rejected; non-admin
/// senders are subject to the per-user mail rate limit.
///
/// Remote actors live in `users` too, but an unqualified lookup must never
/// address one — fediverse DMs are plaintext on every server they touch, so
/// remote addressing is a deliberate, labeled opt-in ([`send_remote`], #110).
pub async fn send_mail(
    pool: &SqlitePool,
    from: &User,
    to_username: &str,
    subject: &str,
    body: &str,
    limits: &Limits,
) -> Result<()> {
    check_sender(pool, from, subject, body, limits).await?;
    let to = auth::find_user(pool, to_username)
        .await?
        .filter(|u| !u.is_remote)
        .ok_or(AppError::RecipientNotFound)?;
    // Honor the recipient's block list (#97) — but a sysop can always be heard.
    if !from.is_admin() && crate::services::blocks::is_blocked(pool, to.id, from.id).await? {
        return Err(AppError::Blocked);
    }
    insert(pool, from.id, to.id, subject, body).await?;
    Ok(())
}

/// Record an outbound **remote** DM to an already-resolved remote actor, and
/// return the new row's id (used to mint the message's ActivityPub URI). The
/// caller ([`crate::web::ap_object::send_remote_dm`]) owns the opt-in gate and
/// the actual delivery; this is the local record + shared sender checks.
pub async fn send_remote(
    pool: &SqlitePool,
    from: &User,
    to_remote: &User,
    subject: &str,
    body: &str,
    limits: &Limits,
) -> Result<i64> {
    check_sender(pool, from, subject, body, limits).await?;
    insert(pool, from.id, to_remote.id, subject, body).await
}

/// Store an inbound **remote** DM: a direct message from a remote actor to a
/// local user (#110). No sender checks apply — a remote server enforces its
/// own — and it's idempotent on the message's `ap_id`, so a redelivery stores
/// once. Returns whether a new row was created.
pub async fn store_inbound_remote(
    pool: &SqlitePool,
    from_remote_id: i64,
    to_local_id: i64,
    subject: &str,
    body: &str,
    ap_id: &str,
) -> Result<bool> {
    let affected = sqlx::query(
        "INSERT INTO mail (from_id, to_id, subject, body, created_at, ap_id) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(ap_id) DO NOTHING",
    )
    .bind(from_remote_id)
    .bind(to_local_id)
    .bind(subject)
    .bind(body)
    .bind(now_unix())
    .bind(ap_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}
