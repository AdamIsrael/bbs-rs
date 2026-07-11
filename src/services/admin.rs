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

/// All registered users, oldest first.
pub async fn list_users(pool: &SqlitePool) -> Result<Vec<User>> {
    let users = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role, created_at, banned_at \
         FROM users ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(users)
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

/// Ban an IP address (upsert reason/time).
pub async fn ban_ip(pool: &SqlitePool, ip: &str, reason: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO ip_bans (ip, reason, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(ip) DO UPDATE SET reason = excluded.reason, created_at = excluded.created_at",
    )
    .bind(ip)
    .bind(reason)
    .bind(now_unix())
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

/// Whether an IP address is currently banned.
pub async fn is_ip_banned(pool: &SqlitePool, ip: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ip_bans WHERE ip = ?")
        .bind(ip)
        .fetch_one(pool)
        .await?;
    Ok(count > 0)
}

/// All IP bans, newest first.
pub async fn list_ip_bans(pool: &SqlitePool) -> Result<Vec<IpBan>> {
    let bans = sqlx::query_as::<_, IpBan>(
        "SELECT ip, reason, created_at FROM ip_bans ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(bans)
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

/// The set of currently-banned IPs — used by the ban sweeper.
pub async fn banned_ips(pool: &SqlitePool) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT ip FROM ip_bans")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}
