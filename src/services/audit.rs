//! Moderation / audit log (#74): an append-only record of who did what, for
//! operator accountability.
//!
//! Entries are written at the call sites that know the acting user — the in-BBS
//! admin/board handlers (actor = the admin's username), `bbsctl` (actor =
//! [`BBSCTL`]), and the ban sweeper (actor = [`SYSTEM`]) — the same pattern as
//! the login audit trail in [`crate::services::admin::record_login`]. Recording
//! is best-effort: a failed write is logged but never fails the action it was
//! meant to describe.

use sqlx::sqlite::SqlitePool;

use crate::db::models::AuditEntry;
use crate::error::Result;
use crate::util::now_unix;

/// Actor value for actions taken through the `bbsctl` operator CLI, which has no
/// logged-in user.
pub const BBSCTL: &str = "bbsctl";

/// Actor value for automated actions (e.g. auto-bans by the abuse sweeper).
pub const SYSTEM: &str = "system";

/// Append one entry. `target` is what was acted on (a username, IP, board name,
/// or post subject); `detail` is optional extra context (a new role, a ban
/// reason, the broadcast text).
pub async fn record(
    pool: &SqlitePool,
    actor: &str,
    action: &str,
    target: &str,
    detail: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (created_at, actor, action, target, detail) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(now_unix())
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}

/// Convenience wrapper that logs a warning instead of returning an error — for
/// call sites where the moderation action has already happened and a failed
/// audit write must not surface as a failure of the action itself.
pub async fn log(pool: &SqlitePool, actor: &str, action: &str, target: &str, detail: Option<&str>) {
    if let Err(e) = record(pool, actor, action, target, detail).await {
        tracing::warn!("audit log write failed ({actor} {action} {target}): {e}");
    }
}

/// Recent audit entries, newest first.
pub async fn recent(pool: &SqlitePool, limit: i64) -> Result<Vec<AuditEntry>> {
    let entries = sqlx::query_as::<_, AuditEntry>(
        "SELECT id, created_at, actor, action, target, detail \
         FROM audit_log ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(entries)
}
