//! User authentication and registration (argon2 password hashing).

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use sqlx::sqlite::SqlitePool;

use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// Hash a plaintext password into a PHC string suitable for storage.
pub fn hash_password(password: &str) -> Result<String> {
    // Draw a random salt from the OS RNG (via `getrandom`, our single RNG
    // source) and B64-encode it for the PHC string.
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| AppError::Hash(e.to_string()))?;
    let salt = SaltString::encode_b64(&raw).map_err(|e| AppError::Hash(e.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Hash(e.to_string()))
}

/// Verify a plaintext password against a stored PHC hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Look up a user by name.
pub async fn find_user(pool: &SqlitePool, username: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role, created_at FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}

/// Return the user iff the password matches. Used by SSH `auth_password`.
pub async fn verify_login(
    pool: &SqlitePool,
    username: &str,
    password: &str,
) -> Result<Option<User>> {
    if let Some(user) = find_user(pool, username).await?
        && verify_password(password, &user.password_hash)
    {
        return Ok(Some(user));
    }
    Ok(None)
}

/// Create a new `user`-role account. Registration is reachable from the guest
/// session so newcomers can bootstrap an account, then reconnect over SSH.
pub async fn register_user(pool: &SqlitePool, username: &str, password: &str) -> Result<User> {
    if find_user(pool, username).await?.is_some() {
        return Err(AppError::UsernameTaken);
    }
    let hash = hash_password(password)?;
    sqlx::query(
        "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, ?, 'user', ?)",
    )
    .bind(username)
    .bind(&hash)
    .bind(now_unix())
    .execute(pool)
    .await?;
    find_user(pool, username).await?.ok_or(AppError::NotFound)
}

/// Ensure the shared `guest/guest` limited account exists.
pub async fn ensure_guest(pool: &SqlitePool) -> Result<()> {
    if find_user(pool, "guest").await?.is_none() {
        let hash = hash_password("guest")?;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('guest', ?, 'guest', ?)")
            .bind(hash)
            .bind(now_unix())
            .execute(pool)
            .await?;
    }
    Ok(())
}
