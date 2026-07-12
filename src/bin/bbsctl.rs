//! bbsctl — operator CLI for managing the bbs-rs database.
//!
//! Operates directly on the SQLite database (the same one the server uses), so
//! it works even when the server is offline. Bans applied here reach live
//! sessions via the server's periodic ban sweeper.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use bbs_rs::config::Settings;
use bbs_rs::db;
use bbs_rs::services::{admin, bulletins};
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
            admin::ban_ip(&pool, &ip, &reason).await?;
            println!("banned ip '{ip}'");
        }
        Cmd::UnbanIp { ip } => {
            admin::unban_ip(&pool, &ip).await?;
            println!("unbanned ip '{ip}'");
        }
        Cmd::IpBans => {
            let bans = admin::list_ip_bans(&pool).await?;
            println!("{:<40} {:<20} REASON", "IP", "WHEN");
            for b in bans {
                println!("{:<40} {:<20} {}", b.ip, fmt_time(b.created_at), b.reason);
            }
        }
        Cmd::Role { username, role } => {
            admin::set_role(&pool, &username, &role).await?;
            println!("set role of '{username}' to '{role}'");
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
