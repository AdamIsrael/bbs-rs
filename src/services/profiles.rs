//! User profiles: the self-description fields on the `users` row (real name,
//! location, tagline, signature) plus a couple of derived stats (last login,
//! post count) assembled for the profile screen. The signature is also shown
//! beneath the author's board posts.

use sqlx::FromRow;
use sqlx::sqlite::SqlitePool;

use crate::error::{AppError, Result};

/// Field length caps (character counts), enforced by [`update_profile`].
pub const MAX_REAL_NAME: usize = 48;
pub const MAX_LOCATION: usize = 48;
pub const MAX_TAGLINE: usize = 80;
pub const MAX_SIGNATURE: usize = 200;

/// A user's public profile: the editable fields plus derived activity stats.
#[derive(Debug, Clone)]
pub struct Profile {
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub created_at: i64,
    pub real_name: String,
    pub location: String,
    pub tagline: String,
    pub signature: String,
    /// Most recent successful login (Unix seconds), or `None` if never.
    pub last_login: Option<i64>,
    /// Number of board messages authored.
    pub post_count: i64,
    /// Whether the user has opted out of the finger service (#77).
    pub finger_optout: bool,
}

/// The editable columns pulled straight from `users`.
#[derive(FromRow)]
struct ProfileRow {
    user_id: i64,
    username: String,
    role: String,
    created_at: i64,
    real_name: String,
    location: String,
    tagline: String,
    signature: String,
    finger_optout: bool,
}

/// Fetch a profile by user id.
pub async fn get_profile(pool: &SqlitePool, user_id: i64) -> Result<Profile> {
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT id AS user_id, username, role, created_at, \
         real_name, location, tagline, signature, finger_optout FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    hydrate(pool, row).await
}

/// Fetch a profile by exact username (as recorded, e.g. from who's-online).
pub async fn get_profile_by_name(pool: &SqlitePool, username: &str) -> Result<Profile> {
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT id AS user_id, username, role, created_at, \
         real_name, location, tagline, signature, finger_optout FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    hydrate(pool, row).await
}

/// Attach the derived stats (last login, post count) to a profile row.
async fn hydrate(pool: &SqlitePool, row: ProfileRow) -> Result<Profile> {
    let last_login: Option<i64> =
        sqlx::query_scalar("SELECT MAX(created_at) FROM logins WHERE username = ? AND success = 1")
            .bind(&row.username)
            .fetch_one(pool)
            .await?;
    let post_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE author_id = ?")
        .bind(row.user_id)
        .fetch_one(pool)
        .await?;
    Ok(Profile {
        user_id: row.user_id,
        username: row.username,
        role: row.role,
        created_at: row.created_at,
        real_name: row.real_name,
        location: row.location,
        tagline: row.tagline,
        signature: row.signature,
        last_login,
        post_count,
        finger_optout: row.finger_optout,
    })
}

/// Toggle a user's finger opt-out (#77), returning the new value. When set, the
/// finger service treats the user as if they don't exist.
pub async fn set_finger_optout(pool: &SqlitePool, user_id: i64, optout: bool) -> Result<()> {
    sqlx::query("UPDATE users SET finger_optout = ? WHERE id = ?")
        .bind(optout as i64)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update a user's editable profile fields. Values are trimmed and length-capped
/// (a too-long field is rejected rather than silently truncated).
pub async fn update_profile(
    pool: &SqlitePool,
    user_id: i64,
    real_name: &str,
    location: &str,
    tagline: &str,
    signature: &str,
) -> Result<()> {
    let real_name = real_name.trim();
    let location = location.trim();
    let tagline = tagline.trim();
    let signature = signature.trim();
    for (label, value, max) in [
        ("Real name", real_name, MAX_REAL_NAME),
        ("Location", location, MAX_LOCATION),
        ("Tagline", tagline, MAX_TAGLINE),
        ("Signature", signature, MAX_SIGNATURE),
    ] {
        if value.chars().count() > max {
            return Err(AppError::FieldTooLong(label, max));
        }
    }
    sqlx::query(
        "UPDATE users SET real_name = ?, location = ?, tagline = ?, signature = ? WHERE id = ?",
    )
    .bind(real_name)
    .bind(location)
    .bind(tagline)
    .bind(signature)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// A user's signature (empty string if unset), for rendering under their posts.
pub async fn signature_of(pool: &SqlitePool, user_id: i64) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT signature FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or_default(),
    )
}
