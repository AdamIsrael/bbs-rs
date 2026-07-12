//! bbsctl — operator CLI for managing the bbs-rs database.
//!
//! Operates directly on the SQLite database (the same one the server uses), so
//! it works even when the server is offline. Bans applied here reach live
//! sessions via the server's periodic ban sweeper.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use bbs_rs::config::Settings;
use bbs_rs::db;
use bbs_rs::services::{admin, auth, boards, bulletins, keys, oneliners};
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
    /// Resolve the database URL: `--database-url` wins, else the config file's
    /// value, else the built-in default.
    fn resolve_database_url(&self) -> String {
        if let Some(url) = &self.database_url {
            return url.clone();
        }
        std::fs::read_to_string(&self.config)
            .ok()
            .and_then(|text| toml::from_str::<Settings>(&text).ok())
            .map(|s| s.network.database_url)
            .unwrap_or_else(|| Settings::default().network.database_url)
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let pool = db::connect(&cli.resolve_database_url()).await?;

    // Operational commands need a current schema, so auto-apply migrations for
    // them. `migrate` controls apply/report itself; `migrate --status` must not
    // apply, so skip the auto-apply for it entirely.
    if !matches!(cli.cmd, Cmd::Migrate { .. }) {
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
    }
    Ok(())
}
