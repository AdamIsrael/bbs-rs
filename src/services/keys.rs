//! SSH public keys registered to users, for public-key authentication.
//!
//! This module is transport-agnostic: it stores and queries already-parsed key
//! material (algorithm / fingerprint / canonical encoding) as opaque strings.
//! Parsing an OpenSSH key line and computing fingerprints lives in
//! [`crate::ssh::pubkey`], which owns the SSH key format.

use sqlx::sqlite::SqlitePool;

use crate::db::models::{User, UserKey};
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// Register a public key for a user. Fields come pre-parsed from
/// [`crate::ssh::pubkey::parse`]. Returns the new row id, or
/// [`AppError::KeyExists`] if the user already has that key.
pub async fn add_key(
    pool: &SqlitePool,
    user_id: i64,
    algorithm: &str,
    fingerprint: &str,
    public_key: &str,
    label: &str,
) -> Result<i64> {
    if is_registered(pool, user_id, fingerprint).await? {
        return Err(AppError::KeyExists);
    }
    let id = sqlx::query(
        "INSERT INTO user_keys (user_id, algorithm, fingerprint, public_key, label, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(algorithm)
    .bind(fingerprint)
    .bind(public_key)
    .bind(label)
    .bind(now_unix())
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Whether `user_id` already has a key with this fingerprint.
async fn is_registered(pool: &SqlitePool, user_id: i64, fingerprint: &str) -> Result<bool> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_keys WHERE user_id = ? AND fingerprint = ?")
            .bind(user_id)
            .bind(fingerprint)
            .fetch_one(pool)
            .await?;
    Ok(n > 0)
}

/// A user's registered keys, newest first.
pub async fn list_keys(pool: &SqlitePool, user_id: i64) -> Result<Vec<UserKey>> {
    let keys = sqlx::query_as::<_, UserKey>(
        "SELECT id, user_id, algorithm, fingerprint, public_key, label, created_at \
         FROM user_keys WHERE user_id = ? ORDER BY id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(keys)
}

/// Delete one of a user's own keys (scoped by `user_id` so a user can only
/// remove their own). Returns whether a row was removed.
pub async fn delete_key(pool: &SqlitePool, user_id: i64, id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM user_keys WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Delete a key by id regardless of owner (operator action via `bbsctl`).
pub async fn delete_key_by_id(pool: &SqlitePool, id: i64) -> Result<bool> {
    let affected = sqlx::query("DELETE FROM user_keys WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Whether `username` has a key with this fingerprint (fast path for the
/// pre-signature `auth_publickey_offered` probe).
pub async fn is_authorized(pool: &SqlitePool, username: &str, fingerprint: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_keys k JOIN users u ON u.id = k.user_id \
         WHERE u.username = ? AND k.fingerprint = ?",
    )
    .bind(username)
    .bind(fingerprint)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// The user who owns this fingerprint under `username`, if any. Used after the
/// SSH signature has been verified to resolve the authenticated account.
pub async fn find_authorized(
    pool: &SqlitePool,
    username: &str,
    fingerprint: &str,
) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT u.id, u.username, u.password_hash, u.role, u.created_at, u.banned_at, \
         u.is_remote \
         FROM users u JOIN user_keys k ON k.user_id = u.id \
         WHERE u.username = ? AND k.fingerprint = ? AND u.is_remote = 0",
    )
    .bind(username)
    .bind(fingerprint)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}
