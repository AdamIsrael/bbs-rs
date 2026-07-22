//! Operator/admin actions: list users, ban/unban (by username and by IP),
//! set roles, and query the login audit trail.
//!
//! These are ungated database operations. Authorization is enforced by the
//! callers — the TUI only exposes the admin menu to `admin`-role users, and
//! `bbsctl` is an operator tool with direct database access.

use std::collections::HashSet;

use sqlx::sqlite::SqlitePool;

use crate::db::models::{IpBan, Login, User};
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// Valid access levels.
pub const ROLES: [&str; 3] = ["guest", "user", "admin"];

/// All registered *local* users, oldest first. Discovered ActivityPub actors
/// also live in `users` (see docs/FEDERATION.md) but they aren't members of
/// this board — they'd swamp the admin list and can't be banned or promoted
/// meaningfully.
pub async fn list_users(pool: &SqlitePool) -> Result<Vec<User>> {
    let users = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role, created_at, banned_at, validated_at, is_remote \
         FROM users WHERE is_remote = 0 ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(users)
}

/// The primary sysop account — the lowest-id `admin` (the operator who set the
/// board up). Used as the recipient for "mail the sysop" (#71). `None` if no
/// admin exists yet.
pub async fn primary_admin(pool: &SqlitePool) -> Result<Option<User>> {
    let admin = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role, created_at, banned_at, validated_at, is_remote \
         FROM users WHERE role = 'admin' AND is_remote = 0 ORDER BY id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(admin)
}

/// Mark a user banned (idempotent; ignores the guest account gracefully).
pub async fn ban_user(pool: &SqlitePool, username: &str) -> Result<()> {
    sqlx::query("UPDATE users SET banned_at = ? WHERE username = ?")
        .bind(now_unix())
        .bind(username)
        .execute(pool)
        .await?;
    Ok(())
}

/// Clear a user's ban.
pub async fn unban_user(pool: &SqlitePool, username: &str) -> Result<()> {
    sqlx::query("UPDATE users SET banned_at = NULL WHERE username = ?")
        .bind(username)
        .execute(pool)
        .await?;
    Ok(())
}

/// List the local accounts still pending sysop approval (#73), oldest first so
/// the queue is worked front-to-back.
pub async fn pending_users(pool: &SqlitePool) -> Result<Vec<User>> {
    let users = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role, created_at, banned_at, validated_at, is_remote \
         FROM users WHERE is_remote = 0 AND validated_at IS NULL ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(users)
}

/// Approve a pending account (#73): stamp `validated_at` so it can log in.
/// Returns whether a still-pending user was activated.
pub async fn validate_user(pool: &SqlitePool, username: &str) -> Result<bool> {
    let affected = sqlx::query(
        "UPDATE users SET validated_at = ? WHERE username = ? AND validated_at IS NULL",
    )
    .bind(now_unix())
    .bind(username)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Reject a pending registration (#73) by deleting it. Only ever removes a
/// still-pending, non-remote account, so an already-active user (or a remote
/// actor) can't be dropped through this path. Returns whether a row was removed.
pub async fn reject_user(pool: &SqlitePool, username: &str) -> Result<bool> {
    let affected = sqlx::query(
        "DELETE FROM users WHERE username = ? AND validated_at IS NULL AND is_remote = 0",
    )
    .bind(username)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Set a user's role. Rejects anything outside [`ROLES`].
pub async fn set_role(pool: &SqlitePool, username: &str, role: &str) -> Result<()> {
    if !ROLES.contains(&role) {
        return Err(AppError::BadRole(role.to_string()));
    }
    let affected = sqlx::query("UPDATE users SET role = ? WHERE username = ?")
        .bind(role)
        .bind(username)
        .execute(pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Ban an IP address (upsert). `expires_at` is `None` for a permanent ban, or a
/// Unix timestamp for a temporary one (used by auto-bans).
pub async fn ban_ip(
    pool: &SqlitePool,
    ip: &str,
    reason: &str,
    expires_at: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ip_bans (ip, reason, created_at, expires_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(ip) DO UPDATE SET reason = excluded.reason, \
         created_at = excluded.created_at, expires_at = excluded.expires_at",
    )
    .bind(ip)
    .bind(reason)
    .bind(now_unix())
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove an IP ban.
pub async fn unban_ip(pool: &SqlitePool, ip: &str) -> Result<()> {
    sqlx::query("DELETE FROM ip_bans WHERE ip = ?")
        .bind(ip)
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether an IP address is currently banned (ignoring expired bans).
pub async fn is_ip_banned(pool: &SqlitePool, ip: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ip_bans WHERE ip = ? AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(ip)
    .bind(now_unix())
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// All active IP bans, newest first (expired bans are excluded).
pub async fn list_ip_bans(pool: &SqlitePool) -> Result<Vec<IpBan>> {
    let bans = sqlx::query_as::<_, IpBan>(
        "SELECT ip, reason, created_at, expires_at FROM ip_bans \
         WHERE expires_at IS NULL OR expires_at > ? ORDER BY created_at DESC",
    )
    .bind(now_unix())
    .fetch_all(pool)
    .await?;
    Ok(bans)
}

/// Delete IP bans whose expiry has passed. Returns how many were removed.
pub async fn purge_expired_ip_bans(pool: &SqlitePool) -> Result<u64> {
    let removed =
        sqlx::query("DELETE FROM ip_bans WHERE expires_at IS NOT NULL AND expires_at <= ?")
            .bind(now_unix())
            .execute(pool)
            .await?
            .rows_affected();
    Ok(removed)
}

/// IPs with at least `max_failures` failed logins since `since` (Unix seconds)
/// that are not already actively banned — the auto-ban candidates.
pub async fn ips_over_failure_threshold(
    pool: &SqlitePool,
    max_failures: i64,
    since: i64,
) -> Result<Vec<String>> {
    let ips: Vec<String> = sqlx::query_scalar(
        "SELECT ip FROM logins \
         WHERE success = 0 AND ip IS NOT NULL AND created_at >= ? \
           AND ip NOT IN (SELECT ip FROM ip_bans WHERE expires_at IS NULL OR expires_at > ?) \
         GROUP BY ip HAVING COUNT(*) >= ?",
    )
    .bind(since)
    .bind(now_unix())
    .bind(max_failures)
    .fetch_all(pool)
    .await?;
    Ok(ips)
}

/// Record a login attempt in the audit trail.
pub async fn record_login(
    pool: &SqlitePool,
    username: &str,
    ip: Option<&str>,
    success: bool,
) -> Result<()> {
    sqlx::query("INSERT INTO logins (username, ip, success, created_at) VALUES (?, ?, ?, ?)")
        .bind(username)
        .bind(ip)
        .bind(success)
        .bind(now_unix())
        .execute(pool)
        .await?;
    Ok(())
}

/// Recent login attempts, newest first, optionally filtered by username.
pub async fn recent_logins(
    pool: &SqlitePool,
    username: Option<&str>,
    limit: i64,
) -> Result<Vec<Login>> {
    let logins = match username {
        Some(u) => {
            sqlx::query_as::<_, Login>(
                "SELECT id, username, ip, success, created_at \
                 FROM logins WHERE username = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(u)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, Login>(
                "SELECT id, username, ip, success, created_at \
                 FROM logins ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(logins)
}

/// The set of currently-banned usernames — used by the ban sweeper.
pub async fn banned_usernames(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT username FROM users WHERE banned_at IS NOT NULL")
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// The set of currently-banned IPs (excluding expired) — used by the ban sweeper.
pub async fn banned_ips(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT ip FROM ip_bans WHERE expires_at IS NULL OR expires_at > ?")
            .bind(now_unix())
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

// ---- Sysop broadcasts (#69) ------------------------------------------------

/// Queue a broadcast for the server to deliver, returning the new row id. Used
/// by `bbsctl broadcast`, which runs in its own process and so can't reach the
/// in-memory presence registry directly — the server's sweeper picks it up.
pub async fn queue_broadcast(pool: &SqlitePool, text: &str) -> Result<i64> {
    let id = sqlx::query("INSERT INTO broadcasts (text, created_at) VALUES (?, ?)")
        .bind(text)
        .bind(now_unix())
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(id)
}

/// The highest broadcast id, or 0 if none. The sweeper seeds its high-water mark
/// with this at startup so broadcasts queued *before* it came up aren't replayed
/// to whoever connects next.
pub async fn latest_broadcast_id(pool: &SqlitePool) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM broadcasts")
            .fetch_one(pool)
            .await?,
    )
}

/// Broadcasts queued after `after_id`, oldest first, as `(id, text)`. The
/// sweeper delivers each and advances its high-water mark to the last id.
pub async fn broadcasts_after(pool: &SqlitePool, after_id: i64) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, text FROM broadcasts WHERE id > ? ORDER BY id",
    )
    .bind(after_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
