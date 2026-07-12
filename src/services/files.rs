//! File areas — browsable download areas backed by on-disk storage.
//!
//! This module owns the **metadata and accounting** (areas, file rows, quota
//! math, upload validation). It does no file I/O itself: callers (`bbsctl`
//! today, an SFTP subsystem later) copy the bytes into the configured
//! `storage_dir` at the `storage_path` this module assigns.

use sqlx::sqlite::SqlitePool;

use crate::config::Files;
use crate::db::models::{FileArea, FileEntry, User};
use crate::error::{AppError, Result};
use crate::util::now_unix;

/// All file areas, in id order.
pub async fn list_areas(pool: &SqlitePool) -> Result<Vec<FileArea>> {
    let areas = sqlx::query_as::<_, FileArea>(
        "SELECT id, name, description, min_read_role, min_write_role, created_at \
         FROM file_areas ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(areas)
}

/// File areas a viewer with `role` may read.
pub async fn list_readable_areas(pool: &SqlitePool, role: &str) -> Result<Vec<FileArea>> {
    Ok(list_areas(pool)
        .await?
        .into_iter()
        .filter(|a| a.can_read(role))
        .collect())
}

/// Fetch a file area by id.
pub async fn get_area(pool: &SqlitePool, id: i64) -> Result<FileArea> {
    sqlx::query_as::<_, FileArea>(
        "SELECT id, name, description, min_read_role, min_write_role, created_at \
         FROM file_areas WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Fetch a file area by name.
pub async fn get_area_by_name(pool: &SqlitePool, name: &str) -> Result<FileArea> {
    sqlx::query_as::<_, FileArea>(
        "SELECT id, name, description, min_read_role, min_write_role, created_at \
         FROM file_areas WHERE name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Create a file area. `min_read`/`min_write` default to guest/user when `None`.
pub async fn add_area(
    pool: &SqlitePool,
    name: &str,
    description: &str,
    min_read: Option<&str>,
    min_write: Option<&str>,
) -> Result<i64> {
    for role in [min_read, min_write].into_iter().flatten() {
        if !crate::services::admin::ROLES.contains(&role) {
            return Err(AppError::BadRole(role.to_string()));
        }
    }
    let id = sqlx::query(
        "INSERT INTO file_areas (name, description, min_read_role, min_write_role, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(description)
    .bind(min_read.unwrap_or("guest"))
    .bind(min_write.unwrap_or("user"))
    .bind(now_unix())
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Delete an empty file area by name. Returns whether a row was removed;
/// refuses (with [`AppError::NotFound`] semantics via `Ok(false)`) is not used
/// — instead errors if the area still contains files.
pub async fn delete_area(pool: &SqlitePool, name: &str) -> Result<bool> {
    let area = match get_area_by_name(pool, name).await {
        Ok(a) => a,
        Err(AppError::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE area_id = ?")
        .bind(area.id)
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Err(AppError::Other(anyhow::anyhow!(
            "area '{name}' still has {count} file(s); remove them first"
        )));
    }
    let affected = sqlx::query("DELETE FROM file_areas WHERE id = ?")
        .bind(area.id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Files in an area, newest first, joined with the uploader's name.
pub async fn list_files(pool: &SqlitePool, area_id: i64) -> Result<Vec<FileEntry>> {
    let files = sqlx::query_as::<_, FileEntry>(
        "SELECT f.id, f.area_id, f.uploader_id, u.username AS uploader_name, \
         f.filename, f.description, f.size, f.storage_path, f.downloads, f.created_at \
         FROM files f JOIN users u ON u.id = f.uploader_id \
         WHERE f.area_id = ? ORDER BY f.id DESC",
    )
    .bind(area_id)
    .fetch_all(pool)
    .await?;
    Ok(files)
}

/// Fetch a single file by id.
pub async fn get_file(pool: &SqlitePool, id: i64) -> Result<FileEntry> {
    sqlx::query_as::<_, FileEntry>(
        "SELECT f.id, f.area_id, f.uploader_id, u.username AS uploader_name, \
         f.filename, f.description, f.size, f.storage_path, f.downloads, f.created_at \
         FROM files f JOIN users u ON u.id = f.uploader_id \
         WHERE f.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Resolve a file within an area by name (newest match wins if names collide).
pub async fn get_file_by_name(
    pool: &SqlitePool,
    area_id: i64,
    filename: &str,
) -> Result<Option<FileEntry>> {
    let file = sqlx::query_as::<_, FileEntry>(
        "SELECT f.id, f.area_id, f.uploader_id, u.username AS uploader_name, \
         f.filename, f.description, f.size, f.storage_path, f.downloads, f.created_at \
         FROM files f JOIN users u ON u.id = f.uploader_id \
         WHERE f.area_id = ? AND f.filename = ? ORDER BY f.id DESC LIMIT 1",
    )
    .bind(area_id)
    .bind(filename)
    .fetch_optional(pool)
    .await?;
    Ok(file)
}

/// Total bytes a user currently stores across all areas.
pub async fn user_usage(pool: &SqlitePool, user_id: i64) -> Result<i64> {
    let total: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM files WHERE uploader_id = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    Ok(total)
}

/// The bare filename (no directory components), for safe storage naming.
fn basename(filename: &str) -> String {
    std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

/// Validate and record a new file for `uploader`, enforcing the extension
/// allowlist, per-file size cap, and (for non-admins) the uploader's storage
/// quota. Returns the recorded row (including the assigned `storage_path`); the
/// caller then writes the bytes to `<storage_dir>/<storage_path>`.
pub async fn add_file(
    pool: &SqlitePool,
    area_id: i64,
    uploader: &User,
    filename: &str,
    description: &str,
    size: i64,
    cfg: &Files,
) -> Result<FileEntry> {
    // The area must exist.
    let _area = get_area(pool, area_id).await?;

    let name = basename(filename);
    if !cfg.extension_allowed(&name) {
        return Err(AppError::ExtensionNotAllowed);
    }
    let size_u = size.max(0) as u64;
    if cfg.max_file_bytes > 0 && size_u > cfg.max_file_bytes {
        return Err(AppError::FileTooLarge(cfg.max_file_bytes));
    }
    // Admins bypass the storage quota — an operator seeding an area (via
    // `bbsctl`, attributed to an admin) shouldn't be capped like a normal user.
    if cfg.user_quota_bytes > 0 && !uploader.is_admin() {
        let used = user_usage(pool, uploader.id).await?.max(0) as u64;
        if used + size_u > cfg.user_quota_bytes {
            return Err(AppError::QuotaExceeded(cfg.user_quota_bytes));
        }
    }

    // Insert first to get an id, then derive a collision-free storage path.
    let id = sqlx::query(
        "INSERT INTO files (area_id, uploader_id, filename, description, size, storage_path, created_at) \
         VALUES (?, ?, ?, ?, ?, '', ?)",
    )
    .bind(area_id)
    .bind(uploader.id)
    .bind(&name)
    .bind(description)
    .bind(size)
    .bind(now_unix())
    .execute(pool)
    .await?
    .last_insert_rowid();

    let storage_path = format!("{id}-{name}");
    sqlx::query("UPDATE files SET storage_path = ? WHERE id = ?")
        .bind(&storage_path)
        .bind(id)
        .execute(pool)
        .await?;

    get_file(pool, id).await
}

/// Delete a file row, returning its `storage_path` so the caller can remove the
/// blob. `None` if there was no such file.
pub async fn delete_file(pool: &SqlitePool, id: i64) -> Result<Option<String>> {
    let path: Option<String> = sqlx::query_scalar("SELECT storage_path FROM files WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    if path.is_some() {
        sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(path)
}

/// Update a file's description. Ungated — callers (the uploader or an admin in
/// the TUI, or `bbsctl`) enforce who may edit. Returns whether a row matched.
pub async fn set_description(pool: &SqlitePool, id: i64, description: &str) -> Result<bool> {
    let affected = sqlx::query("UPDATE files SET description = ? WHERE id = ?")
        .bind(description)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Increment a file's download counter.
pub async fn record_download(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("UPDATE files SET downloads = downloads + 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Seed a default file area on first run.
pub async fn ensure_default_areas(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_areas")
        .fetch_one(pool)
        .await?;
    if count == 0 {
        add_area(
            pool,
            "Uploads",
            "General user uploads",
            Some("guest"),
            Some("user"),
        )
        .await?;
    }
    Ok(())
}
