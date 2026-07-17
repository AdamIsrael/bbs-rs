//! User authentication and registration (argon2 password hashing).

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use sqlx::sqlite::SqlitePool;

use crate::config::Accounts;
use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::services::admin;
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
        "SELECT id, username, password_hash, role, created_at, banned_at, is_remote \
         FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}

/// Return the user iff the password matches. Used by SSH `auth_password`.
///
/// Remote ActivityPub actors are rejected outright. Their stored hash is an
/// unusable sentinel so verification would fail anyway, but that's a safety
/// net, not the rule — say it explicitly.
pub async fn verify_login(
    pool: &SqlitePool,
    username: &str,
    password: &str,
) -> Result<Option<User>> {
    if let Some(user) = find_user(pool, username).await?
        && !user.is_remote
        && verify_password(password, &user.password_hash)
    {
        return Ok(Some(user));
    }
    Ok(None)
}

/// The full login decision used by the SSH handler: reject banned IPs and
/// banned accounts, verify the password, and record every attempt (success or
/// failure) in the `logins` audit table. Returns the authenticated user, or
/// `None` for any rejection.
pub async fn attempt_login(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    ip: Option<&str>,
) -> Result<Option<User>> {
    // Reject connections from a banned IP outright.
    if let Some(ip) = ip
        && admin::is_ip_banned(pool, ip).await?
    {
        admin::record_login(pool, username, Some(ip), false).await?;
        return Ok(None);
    }

    let outcome = match verify_login(pool, username, password).await? {
        Some(user) if !user.is_banned() => Some(user),
        _ => None,
    };

    admin::record_login(pool, username, ip, outcome.is_some()).await?;
    Ok(outcome)
}

/// The public-key counterpart to [`attempt_login`]: authenticate `username` by
/// a verified SSH key `fingerprint`. russh has already checked the client owns
/// the key (signature verified) before this is called. Rejects banned IPs and
/// banned accounts, and records every attempt in the audit trail.
pub async fn attempt_pubkey_login(
    pool: &SqlitePool,
    username: &str,
    fingerprint: &str,
    ip: Option<&str>,
) -> Result<Option<User>> {
    if let Some(ip) = ip
        && admin::is_ip_banned(pool, ip).await?
    {
        admin::record_login(pool, username, Some(ip), false).await?;
        return Ok(None);
    }

    let outcome = match crate::services::keys::find_authorized(pool, username, fingerprint).await? {
        Some(user) if !user.is_banned() => Some(user),
        _ => None,
    };

    admin::record_login(pool, username, ip, outcome.is_some()).await?;
    Ok(outcome)
}

/// The longest username we'll register. Keeps handles renderable in the TUI and
/// well within what remote servers accept as an actor's `preferredUsername`.
pub const MAX_USERNAME_CHARS: usize = 32;

/// Whether `c` may appear in a new username: ASCII letters/digits plus `_ - .`.
///
/// This is deliberately narrow, and it is a **security boundary**, not style.
/// Federated actors are stored in `users` keyed by a fully-qualified
/// `alice@remote.social` handle (see docs/FEDERATION.md), so a local account
/// containing `@` could impersonate a remote one. `/` would likewise break out
/// of an actor URI path, and whitespace/control characters corrupt rendering
/// and WebFinger lookups.
fn username_char_ok(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Validate a username for registration. Applies to **new** accounts only, so
/// pre-existing rows (and the seeded `guest`) are untouched.
pub fn validate_username(username: &str) -> Result<()> {
    let invalid = || AppError::UsernameInvalid(MAX_USERNAME_CHARS);
    // Compare against the raw input: a name that only differs by surrounding
    // whitespace would otherwise collide with an existing one on lookup.
    if username != username.trim() {
        return Err(invalid());
    }
    let len = username.chars().count();
    if len == 0 || len > MAX_USERNAME_CHARS {
        return Err(invalid());
    }
    if !username.chars().all(username_char_ok) {
        return Err(invalid());
    }
    Ok(())
}

/// Create a new `user`-role account. Registration is reachable from the guest
/// session so newcomers can bootstrap an account, then reconnect over SSH.
///
/// Rejects reserved usernames (see [`Accounts::is_reserved`]) so bots can't
/// grab `root`/`admin` and so `guest` stays the shared account, and enforces
/// [`validate_username`] so a local account can't impersonate a federated
/// `user@domain` handle.
pub async fn register_user(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    accounts: &Accounts,
) -> Result<User> {
    // Reserved-name policy first: `is_reserved` deliberately trims and folds
    // case, so "  Root  " should report *why* it's refused rather than being
    // caught by the character rule below on its whitespace.
    if accounts.is_reserved(username) {
        return Err(AppError::UsernameReserved);
    }
    validate_username(username)?;
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

/// Ensure the shared `guest` limited account exists, with the given password
/// (defaults to "guest" via `[seed] guest_password`).
pub async fn ensure_guest(pool: &SqlitePool, password: &str) -> Result<()> {
    if find_user(pool, "guest").await?.is_none() {
        let hash = hash_password(password)?;
        sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES ('guest', ?, 'guest', ?)")
            .bind(hash)
            .bind(now_unix())
            .execute(pool)
            .await?;
    }
    Ok(())
}
