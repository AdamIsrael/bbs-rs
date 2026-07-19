//! Whether `[seed]` will actually do anything (#147).
//!
//! Seeding is **first-run only**: `boards::ensure_default_boards` inserts the
//! configured boards only when the `boards` table is empty. On a database that
//! already has boards, editing `[seed] boards` changes the file and changes
//! nothing else — the next start ignores it.
//!
//! A UI that silently accepts edits with no effect is worse than one that
//! doesn't offer them. `bbscfg` can tell, because the database is named right
//! there in `[network] database_url`, so it checks and says so.

use std::path::{Path, PathBuf};

/// What editing `[seed]` will do, given the configured database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedStatus {
    /// No database yet, or an empty `boards` table — seeding will run on the
    /// next start. Edits here take effect.
    WillSeed,
    /// The database already has boards, so seeding is skipped. Edits here are
    /// recorded but do nothing until the database is recreated.
    AlreadySeeded { boards: i64 },
    /// The database URL isn't a local SQLite file we can inspect (or the query
    /// failed). We don't guess — better to say we can't tell than to imply
    /// either answer.
    Unknown { reason: String },
}

/// Resolve the on-disk path of a `sqlite://…` URL, relative to the config's
/// directory (where the server's working directory will be). Returns `None`
/// for a non-file URL like `sqlite::memory:`.
fn sqlite_path(url: &str, config_dir: &Path) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    // Drop the query string (`?mode=rwc` etc).
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path.starts_with(':') {
        return None; // `sqlite::memory:` and friends
    }
    let p = Path::new(path);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_dir.join(p)
    })
}

/// Check whether `[seed]` edits will take effect.
///
/// **Never creates the database.** It connects read-only and only when the file
/// already exists — opening the config must not have the side effect of
/// bringing a database into being. A missing file, or a database not yet
/// migrated (no `boards` table), both count as `WillSeed`, because that's what
/// the server will find on first run.
pub fn status(database_url: &str, config_dir: &Path) -> SeedStatus {
    let Some(path) = sqlite_path(database_url, config_dir) else {
        return SeedStatus::Unknown {
            reason: format!("{database_url:?} is not a local SQLite file"),
        };
    };
    if !path.exists() {
        return SeedStatus::WillSeed;
    }

    // A short-lived current-thread runtime: bbscfg is otherwise sync, and this
    // is a single fast query. Read-only and create_if_missing(false) guarantee
    // we can't alter or create the file.
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return SeedStatus::Unknown {
                reason: format!("runtime: {e}"),
            };
        }
    };

    rt.block_on(async move {
        let opts = match SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())) {
            Ok(o) => o.read_only(true).create_if_missing(false),
            Err(e) => {
                return SeedStatus::Unknown {
                    reason: e.to_string(),
                };
            }
        };
        let pool = match SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                return SeedStatus::Unknown {
                    reason: e.to_string(),
                };
            }
        };
        // A database that exists but hasn't been migrated has no `boards`
        // table; that query errors, which we read as "nothing seeded yet".
        match sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM boards")
            .fetch_one(&pool)
            .await
        {
            Ok(0) => SeedStatus::WillSeed,
            Ok(n) => SeedStatus::AlreadySeeded { boards: n },
            Err(_) => SeedStatus::WillSeed,
        }
    })
}
