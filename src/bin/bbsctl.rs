//! bbsctl — operator CLI for managing the bbs-rs database.
//!
//! Operates directly on the SQLite database (the same one the server uses), so
//! it works even when the server is offline. Bans applied here reach live
//! sessions via the server's periodic ban sweeper.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use bbs_rs::config::Settings;
use bbs_rs::db;
use bbs_rs::services::{admin, auth, boards, bulletins, files, keys, oneliners};
use bbs_rs::ssh::pubkey;
use bbs_rs::util::fmt_time;

#[derive(Parser)]
#[command(
    name = "bbsctl",
    about = "Manage bbs-rs: users, bans, and login history"
)]
struct Cli {
    /// Config file to read the database URL from (must match the server's).
    #[arg(long, default_value = "bbs.toml")]
    config: PathBuf,

    /// SQLite database URL. Overrides the value from the config file.
    #[arg(long)]
    database_url: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    /// Load the full settings from the config file (defaults if missing/invalid).
    /// Used for file-area commands that need `[files]` (storage dir + limits).
    fn load_settings(&self) -> Settings {
        std::fs::read_to_string(&self.config)
            .ok()
            .and_then(|text| toml::from_str::<Settings>(&text).ok())
            .unwrap_or_default()
    }

    /// Resolve the database URL: `--database-url` wins, else the config file's
    /// value, else the built-in default.
    fn resolve_database_url(&self) -> String {
        if let Some(url) = &self.database_url {
            return url.clone();
        }
        self.load_settings().network.database_url
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply pending database migrations (or show status with --status).
    Migrate {
        /// Show applied/pending migrations without applying anything.
        #[arg(long)]
        status: bool,
    },
    /// List all registered users.
    Users,
    /// Ban a user by username.
    Ban { username: String },
    /// Lift a user's ban.
    Unban { username: String },
    /// Ban an IP address.
    BanIp {
        ip: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Lift an IP ban.
    UnbanIp { ip: String },
    /// List banned IP addresses.
    IpBans,
    /// Set a user's role (guest | user | admin).
    Role { username: String, role: String },
    /// List a user's registered SSH public keys.
    Keys { username: String },
    /// Register an SSH public key for a user (pass the key line, or --file).
    AddKey {
        username: String,
        /// The public-key line ("ssh-ed25519 AAAA… comment").
        key: Option<String>,
        /// Read the key from a file instead of the positional argument.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Optional label (defaults to the key's comment).
        #[arg(long, default_value = "")]
        label: String,
    },
    /// Remove a registered SSH public key by its id.
    RmKey { id: i64 },
    /// List boards with their read/write ACLs and lock state.
    Boards,
    /// Configure a board's ACLs and/or lock state.
    SetBoard {
        /// Board name (as shown by `boards`).
        name: String,
        /// Minimum role to read (guest | user | admin).
        #[arg(long)]
        read: Option<String>,
        /// Minimum role to post (guest | user | admin).
        #[arg(long)]
        write: Option<String>,
        /// Lock the board (reject new posts).
        #[arg(long)]
        lock: bool,
        /// Unlock the board.
        #[arg(long)]
        unlock: bool,
    },
    /// List file areas with their ACLs.
    FileAreas,
    /// Create a file area.
    AddArea {
        name: String,
        #[arg(long, default_value = "")]
        desc: String,
        /// Minimum role to view/download (guest | user | admin).
        #[arg(long)]
        read: Option<String>,
        /// Minimum role to upload (guest | user | admin).
        #[arg(long)]
        write: Option<String>,
    },
    /// Remove an empty file area by name.
    RmArea { name: String },
    /// List files in an area.
    Files { area: String },
    /// Add a file to an area from a server path, attributed to a user.
    AddFile {
        area: String,
        user: String,
        path: PathBuf,
        #[arg(long, default_value = "")]
        desc: String,
    },
    /// Remove a file by id (deletes the stored blob too).
    RmFile { id: i64 },
    /// Set a file's description by id (SFTP uploads start with none).
    SetFileDesc { id: i64, description: String },
    /// List sysop bulletins.
    Bulletins,
    /// Post a new sysop bulletin (shown to users after login).
    PostBulletin {
        title: String,
        #[arg(long)]
        body: String,
    },
    /// Remove a bulletin by id.
    RmBulletin { id: i64 },
    /// List recent oneliners (graffiti wall).
    Oneliners {
        /// Maximum rows to show.
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Remove a oneliner by id (moderation).
    RmOneliner { id: i64 },
    /// List the ActivityPub federation allow/block domains.
    ApPeers,
    /// Allow a domain to federate (needed in the default allowlist posture).
    ApAllow {
        domain: String,
        #[arg(default_value = "")]
        reason: String,
    },
    /// Block a domain from federating (used in blocklist posture).
    ApBlock {
        domain: String,
        #[arg(default_value = "")]
        reason: String,
    },
    /// Remove a domain's allow entry.
    ApUnallow { domain: String },
    /// Remove a domain's block entry.
    ApUnblock { domain: String },
    /// Show recent login attempts.
    Logins {
        /// Filter to a single username.
        #[arg(long)]
        user: Option<String>,
        /// Maximum rows to show.
        #[arg(long, default_value_t = 20)]
        limit: i64,
        /// Show only failed attempts.
        #[arg(long)]
        failures: bool,
    },
    /// Snapshot the database (and optionally the file blobs) into a directory.
    /// Uses SQLite's online VACUUM INTO, so it's safe while the server runs.
    Backup {
        /// Directory to write the backup into (created if missing).
        #[arg(long, default_value = "backups")]
        out: PathBuf,
        /// Also copy the file-area storage directory.
        #[arg(long)]
        files: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let settings = cli.load_settings();
    let pool = db::connect(&cli.resolve_database_url()).await?;

    // Operational commands need a current schema, so auto-apply migrations for
    // them. `migrate` controls apply/report itself; `migrate --status` must not
    // apply, so skip the auto-apply for it entirely. `backup` is read-only and
    // must not mutate the live DB, so it's skipped too.
    if !matches!(cli.cmd, Cmd::Migrate { .. } | Cmd::Backup { .. }) {
        db::run_migrations(&pool).await?;
    }

    match cli.cmd {
        Cmd::Migrate { status } => {
            if status {
                let rows = db::migration_status(&pool).await?;
                println!("{:<5} {:<8} DESCRIPTION", "VER", "STATUS");
                for r in rows {
                    let st = if r.applied { "applied" } else { "pending" };
                    println!("{:<5} {:<8} {}", r.version, st, r.description);
                }
            } else {
                let newly = db::run_migrations_reporting(&pool).await?;
                if newly.is_empty() {
                    println!("database is up to date");
                } else {
                    for v in &newly {
                        println!("applied migration {v}");
                    }
                    println!("applied {} migration(s)", newly.len());
                }
            }
        }
        Cmd::Users => {
            let users = admin::list_users(&pool).await?;
            println!("{:<20} {:<8} {:<8} CREATED", "USERNAME", "ROLE", "STATUS");
            for u in users {
                let status = if u.is_banned() { "banned" } else { "ok" };
                println!(
                    "{:<20} {:<8} {:<8} {}",
                    u.username,
                    u.role,
                    status,
                    fmt_time(u.created_at)
                );
            }
        }
        Cmd::Ban { username } => {
            admin::ban_user(&pool, &username).await?;
            println!("banned user '{username}'");
        }
        Cmd::Unban { username } => {
            admin::unban_user(&pool, &username).await?;
            println!("unbanned user '{username}'");
        }
        Cmd::BanIp { ip, reason } => {
            // Manual bans are permanent (no expiry).
            admin::ban_ip(&pool, &ip, &reason, None).await?;
            println!("banned ip '{ip}'");
        }
        Cmd::UnbanIp { ip } => {
            admin::unban_ip(&pool, &ip).await?;
            println!("unbanned ip '{ip}'");
        }
        Cmd::IpBans => {
            let bans = admin::list_ip_bans(&pool).await?;
            println!("{:<40} {:<20} {:<20} REASON", "IP", "WHEN", "EXPIRES");
            for b in bans {
                let expires = b.expires_at.map(fmt_time).unwrap_or_else(|| "never".into());
                println!(
                    "{:<40} {:<20} {:<20} {}",
                    b.ip,
                    fmt_time(b.created_at),
                    expires,
                    b.reason
                );
            }
        }
        Cmd::Role { username, role } => {
            admin::set_role(&pool, &username, &role).await?;
            println!("set role of '{username}' to '{role}'");
        }
        Cmd::Keys { username } => {
            let user = auth::find_user(&pool, &username)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no such user: {username}"))?;
            let list = keys::list_keys(&pool, user.id).await?;
            println!("{:<5} {:<12} {:<20} FINGERPRINT", "ID", "ALGO", "LABEL");
            for k in list {
                let label = if k.label.is_empty() { "-" } else { &k.label };
                println!(
                    "{:<5} {:<12} {:<20} {}",
                    k.id, k.algorithm, label, k.fingerprint
                );
            }
        }
        Cmd::AddKey {
            username,
            key,
            file,
            label,
        } => {
            let user = auth::find_user(&pool, &username)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no such user: {username}"))?;
            let line = match (key, file) {
                (Some(k), None) => k,
                (None, Some(path)) => std::fs::read_to_string(&path)?,
                (Some(_), Some(_)) => {
                    anyhow::bail!("pass either a key argument or --file, not both")
                }
                (None, None) => anyhow::bail!("provide the key line or --file <path>"),
            };
            let parsed = pubkey::register(&pool, user.id, &line, &label).await?;
            println!(
                "added {} key for '{username}' ({})",
                parsed.algorithm, parsed.fingerprint
            );
        }
        Cmd::RmKey { id } => {
            if keys::delete_key_by_id(&pool, id).await? {
                println!("removed key #{id}");
            } else {
                println!("no key #{id}");
            }
        }
        Cmd::Boards => {
            let list = boards::list_boards(&pool).await?;
            println!(
                "{:<18} {:<8} {:<8} {:<8} DESCRIPTION",
                "NAME", "READ", "WRITE", "LOCKED"
            );
            for b in list {
                println!(
                    "{:<18} {:<8} {:<8} {:<8} {}",
                    b.name,
                    b.min_read_role,
                    b.min_write_role,
                    if b.locked { "yes" } else { "no" },
                    b.description
                );
            }
        }
        Cmd::SetBoard {
            name,
            read,
            write,
            lock,
            unlock,
        } => {
            if lock && unlock {
                anyhow::bail!("--lock and --unlock are mutually exclusive");
            }
            if read.is_some() || write.is_some() {
                boards::set_roles(&pool, &name, read.as_deref(), write.as_deref()).await?;
            }
            if lock || unlock {
                boards::set_locked_by_name(&pool, &name, lock).await?;
            }
            println!("updated board '{name}'");
        }
        Cmd::FileAreas => {
            let areas = files::list_areas(&pool).await?;
            println!("{:<18} {:<8} {:<8} DESCRIPTION", "NAME", "READ", "WRITE");
            for a in areas {
                println!(
                    "{:<18} {:<8} {:<8} {}",
                    a.name, a.min_read_role, a.min_write_role, a.description
                );
            }
        }
        Cmd::AddArea {
            name,
            desc,
            read,
            write,
        } => {
            files::add_area(&pool, &name, &desc, read.as_deref(), write.as_deref()).await?;
            println!("created file area '{name}'");
        }
        Cmd::RmArea { name } => {
            if files::delete_area(&pool, &name).await? {
                println!("removed file area '{name}'");
            } else {
                println!("no file area '{name}'");
            }
        }
        Cmd::Files { area } => {
            let a = files::get_area_by_name(&pool, &area).await?;
            let list = files::list_files(&pool, a.id).await?;
            println!(
                "{:<5} {:<26} {:>10} {:<12} {:>5} DESCRIPTION",
                "ID", "FILENAME", "SIZE", "UPLOADER", "DL"
            );
            for file in list {
                println!(
                    "{:<5} {:<26} {:>10} {:<12} {:>5} {}",
                    file.id,
                    file.filename,
                    file.size,
                    file.uploader_name,
                    file.downloads,
                    file.description
                );
            }
        }
        Cmd::AddFile {
            area,
            user,
            path,
            desc,
        } => {
            let a = files::get_area_by_name(&pool, &area).await?;
            let u = auth::find_user(&pool, &user)
                .await?
                .ok_or_else(|| anyhow::anyhow!("no such user: {user}"))?;
            let bytes = std::fs::read(&path)?;
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow::anyhow!("bad file path: {}", path.display()))?;
            // Record metadata (validates extension / size / quota), then write
            // the blob; roll the row back if the write fails.
            let entry = files::add_file(
                &pool,
                a.id,
                &u,
                filename,
                &desc,
                bytes.len() as i64,
                &settings.files,
            )
            .await?;
            let dest = settings.files.storage_dir.join(&entry.storage_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Err(e) = std::fs::write(&dest, &bytes) {
                let _ = files::delete_file(&pool, entry.id).await;
                return Err(e.into());
            }
            println!(
                "added '{}' ({} bytes) to '{area}' as #{} for '{user}'",
                entry.filename, entry.size, entry.id
            );
        }
        Cmd::RmFile { id } => match files::delete_file(&pool, id).await? {
            Some(storage_path) => {
                let _ = std::fs::remove_file(settings.files.storage_dir.join(&storage_path));
                println!("removed file #{id}");
            }
            None => println!("no file #{id}"),
        },
        Cmd::SetFileDesc { id, description } => {
            if files::set_description(&pool, id, &description).await? {
                println!("updated description of file #{id}");
            } else {
                println!("no file #{id}");
            }
        }
        Cmd::Bulletins => {
            let list = bulletins::list(&pool).await?;
            println!("{:<5} {:<20} TITLE", "ID", "WHEN");
            for b in list {
                println!("{:<5} {:<20} {}", b.id, fmt_time(b.created_at), b.title);
            }
        }
        Cmd::PostBulletin { title, body } => {
            let id = bulletins::add(&pool, &title, &body).await?;
            println!("posted bulletin #{id}");
        }
        Cmd::RmBulletin { id } => {
            if bulletins::delete(&pool, id).await? {
                println!("removed bulletin #{id}");
            } else {
                println!("no bulletin #{id}");
            }
        }
        Cmd::Oneliners { limit } => {
            let list = oneliners::recent(&pool, limit).await?;
            println!("{:<5} {:<20} {:<14} TEXT", "ID", "WHEN", "AUTHOR");
            for o in list {
                println!(
                    "{:<5} {:<20} {:<14} {}",
                    o.id,
                    fmt_time(o.created_at),
                    o.author_name,
                    o.body
                );
            }
        }
        Cmd::RmOneliner { id } => {
            if oneliners::delete(&pool, id).await? {
                println!("removed oneliner #{id}");
            } else {
                println!("no oneliner #{id}");
            }
        }
        Cmd::ApPeers => {
            use bbs_rs::services::federation::policy;
            for kind in ["allow", "block"] {
                let rows = policy::list(&pool, kind).await?;
                if rows.is_empty() {
                    println!("({kind}: none)");
                } else {
                    println!("{}:", kind.to_uppercase());
                    for (domain, reason) in rows {
                        println!("  {domain:<30} {reason}");
                    }
                }
            }
        }
        Cmd::ApAllow { domain, reason } => {
            bbs_rs::services::federation::policy::set(&pool, &domain, "allow", &reason).await?;
            println!("allowed {domain}");
        }
        Cmd::ApBlock { domain, reason } => {
            bbs_rs::services::federation::policy::set(&pool, &domain, "block", &reason).await?;
            println!("blocked {domain}");
        }
        Cmd::ApUnallow { domain } => {
            let removed =
                bbs_rs::services::federation::policy::unset(&pool, &domain, "allow").await?;
            println!(
                "{}",
                if removed {
                    format!("removed allow for {domain}")
                } else {
                    format!("no allow entry for {domain}")
                }
            );
        }
        Cmd::ApUnblock { domain } => {
            let removed =
                bbs_rs::services::federation::policy::unset(&pool, &domain, "block").await?;
            println!(
                "{}",
                if removed {
                    format!("removed block for {domain}")
                } else {
                    format!("no block entry for {domain}")
                }
            );
        }
        Cmd::Logins {
            user,
            limit,
            failures,
        } => {
            let logins = admin::recent_logins(&pool, user.as_deref(), limit).await?;
            println!("{:<20} {:<20} {:<8} IP", "WHEN", "USERNAME", "RESULT");
            for l in logins {
                if failures && l.success {
                    continue;
                }
                let result = if l.success { "ok" } else { "reject" };
                println!(
                    "{:<20} {:<20} {:<8} {}",
                    fmt_time(l.created_at),
                    l.username,
                    result,
                    l.ip.as_deref().unwrap_or("-")
                );
            }
        }
        Cmd::Backup { out, files } => {
            std::fs::create_dir_all(&out)
                .with_context(|| format!("creating backup dir {}", out.display()))?;
            let stamp = backup_stamp();

            // Database snapshot (online, consistent).
            let db_dest = out.join(format!("bbs-{stamp}.db"));
            if db_dest.exists() {
                anyhow::bail!("backup target {} already exists", db_dest.display());
            }
            db::backup_into(&pool, &db_dest).await?;
            let size = std::fs::metadata(&db_dest).map(|m| m.len()).unwrap_or(0);
            println!("database -> {} ({size} bytes)", db_dest.display());

            // Optionally, the file-area blobs.
            if files {
                let src = &settings.files.storage_dir;
                if src.is_dir() {
                    let dst = out.join(format!("files-{stamp}"));
                    let (n, bytes) = copy_dir_all(src, &dst)?;
                    println!("files    -> {} ({n} files, {bytes} bytes)", dst.display());
                } else {
                    println!("files    -> (none; {} does not exist)", src.display());
                }
            }
        }
    }
    Ok(())
}

/// A filesystem-safe UTC timestamp for backup names, e.g. `20260715-011530`.
fn backup_stamp() -> String {
    let dt = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Recursively copy `src` into `dst`, returning (files copied, total bytes).
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<(u64, u64)> {
    std::fs::create_dir_all(dst)?;
    let (mut files, mut bytes) = (0u64, 0u64);
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            let (f, b) = copy_dir_all(&from, &to)?;
            files += f;
            bytes += b;
        } else {
            bytes += std::fs::copy(&from, &to)?;
            files += 1;
        }
    }
    Ok((files, bytes))
}
