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
use bbs_rs::services::{admin, audit, auth, boards, bulletins, files, keys, oneliners, polls};
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
    /// Load the full settings from the config file. A missing file yields the
    /// built-in defaults (the server writes one on first run, so bbsctl may run
    /// before it exists); a file that exists but can't be read or parsed is a
    /// hard error, since silently falling back would point commands like
    /// `migrate` at a different database than the operator intended.
    /// Used for file-area commands that need `[files]` (storage dir + limits).
    fn load_settings(&self) -> anyhow::Result<Settings> {
        let text = match std::fs::read_to_string(&self.config) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => {
                return Err(e).with_context(|| format!("reading config {}", self.config.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing config {}", self.config.display()))
    }

    /// Resolve the database URL: `--database-url` wins, else the config file's
    /// value, else the built-in default.
    fn resolve_database_url(&self, settings: &Settings) -> String {
        if let Some(url) = &self.database_url {
            return url.clone();
        }
        settings.network.database_url.clone()
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
    /// List accounts pending sysop approval (new-user validation queue).
    Pending,
    /// Approve a pending account, letting it log in.
    Approve { username: String },
    /// Reject (delete) a pending registration.
    Reject { username: String },
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
    /// Broadcast a message to every live session (e.g. a maintenance notice).
    Broadcast { message: String },
    /// Set a user's role (guest | user | admin).
    Role { username: String, role: String },
    /// Reset a user's password (#76). With no --password, a strong temporary
    /// one is generated and printed — hand it to the user out of band. Unless
    /// --no-force is given, their next login must set a new password before
    /// anything else is reachable.
    Passwd {
        username: String,
        /// Set this exact password instead of generating one.
        #[arg(long)]
        password: Option<String>,
        /// Leave the password as-is on next login (no forced change).
        #[arg(long)]
        no_force: bool,
    },
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
    /// List polls with their vote totals and open/closed state.
    Polls {
        /// Maximum rows to show.
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Close a poll by id (stops further voting; results stay visible).
    ClosePoll { id: i64 },
    /// Remove a poll by id, along with its options and votes (moderation).
    RmPoll { id: i64 },
    /// List the ActivityPub federation allow/block domains.
    ApPeers,
    /// Allow a domain to federate (needed in the default allowlist posture).
    ApAllow {
        domain: String,
        #[arg(default_value = "")]
        reason: String,
    },
    /// Block a domain. `--severity suspend` (default) refuses it entirely;
    /// `silence` still lets it federate but stops accepting its content into
    /// boards, the timeline, and mirrors.
    ApBlock {
        domain: String,
        #[arg(default_value = "")]
        reason: String,
        #[arg(long, default_value = "suspend", value_parser = ["suspend", "silence"])]
        severity: String,
    },
    /// Remove a domain's allow entry.
    ApUnallow { domain: String },
    /// Remove a domain's block entry.
    ApUnblock { domain: String },
    /// Follow a remote account (`user@host`) on behalf of a local user. Resolves
    /// the handle over WebFinger and sends a signed `Follow`.
    ApFollow {
        /// The local user doing the following.
        user: String,
        /// The remote handle, e.g. `alice@mastodon.social`.
        handle: String,
    },
    /// Unfollow a remote account a local user follows.
    ApUnfollow { user: String, handle: String },
    /// List the remote accounts a local user follows, with each follow's state.
    ApFollowing { user: String },
    /// Show cached posts from a followed remote board. Pass the board's handle
    /// (`slug@host`) or its Group actor URI. Follow a board first with
    /// `ap-follow <user> <slug@host>`.
    ApBoardPosts {
        board: String,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Show reports other instances have sent us (open ones by default).
    ApReports {
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Mark a report handled.
    ApResolve { id: i64 },
    /// Delete everything a domain sent us. Blocking is NOT retroactive — this
    /// is the deliberate cleanup step for content that already arrived.
    ApPurge {
        domain: String,
        /// Required: this deletes stored content and cannot be undone.
        #[arg(long)]
        yes: bool,
    },
    /// Show the moderation / audit log (who did what).
    Audit {
        /// Maximum rows to show.
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
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
    /// Check the configured menu (#86) for problems: unknown actions, dangling
    /// door/board/submenu targets, duplicate hotkeys within a level, submenu
    /// cycles, and unreachable submenus. Exits non-zero if any error is found.
    ValidateMenu,
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

/// Resolve a local user that is allowed to federate (exists, not remote, not the
/// shared guest).
async fn local_federatable_user(
    pool: &sqlx::SqlitePool,
    username: &str,
) -> anyhow::Result<bbs_rs::db::models::User> {
    let user = auth::find_user(pool, username)
        .await?
        .with_context(|| format!("no such user {username:?}"))?;
    anyhow::ensure!(
        !user.is_remote && !user.is_guest(),
        "{username} cannot federate (guests and remote actors don't have follows)"
    );
    Ok(user)
}

/// The hotkey an entry would resolve to (#86), mirroring `build_menu_group`:
/// the operator's `key`, else the target's default (a built-in's key, or the
/// first letter of a compound target's label/name). Used to spot collisions.
fn menu_effective_key(
    e: &bbs_rs::config::MenuEntry,
    action: &bbs_rs::app::state::MenuAction,
) -> Option<char> {
    use bbs_rs::app::state::MenuAction;
    if let Some(c) = e.key.chars().next() {
        return Some(c);
    }
    let label = if e.label.trim().is_empty() {
        match action {
            MenuAction::Builtin(i) => i.label().to_string(),
            MenuAction::Door(n) | MenuAction::Board(n) | MenuAction::Submenu(n) => n.clone(),
        }
    } else {
        e.label.trim().to_string()
    };
    action.default_key(&label)
}

/// Validate the configured menu tree (#86). Prints every problem found and
/// returns whether the config is error-free (warnings don't count as failure).
async fn validate_menu(
    settings: &bbs_rs::config::Settings,
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<bool> {
    use bbs_rs::app::state::MenuAction;
    use std::collections::{HashMap, HashSet};

    let board_names: HashSet<String> = boards::list_boards(pool)
        .await?
        .into_iter()
        .map(|b| b.name)
        .collect();
    let door_names: HashSet<&str> = settings.doors.iter().map(|d| d.name.as_str()).collect();
    let submenu_names: HashSet<&str> = settings.submenus.keys().map(|s| s.as_str()).collect();

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Validate one named group, returning the submenu names it references.
    let validate_group = |group: &str,
                          entries: &[bbs_rs::config::MenuEntry],
                          errors: &mut Vec<String>,
                          warnings: &mut Vec<String>|
     -> Vec<String> {
        let mut refs = Vec::new();
        let mut keys: HashMap<char, String> = HashMap::new();
        // An empty top-level menu is the "use the built-in default" signal, so
        // only an empty *submenu* (which would resolve to nothing) is notable.
        if entries.is_empty() && group != "menu" {
            warnings.push(format!("{group}: has no entries"));
        }
        for e in entries {
            let Some(action) = MenuAction::parse(&e.action) else {
                errors.push(format!("{group}: unknown action {:?}", e.action));
                continue;
            };
            match &action {
                MenuAction::Door(n) if !door_names.contains(n.as_str()) => {
                    errors.push(format!("{group}: door target {n:?} has no [[doors]] entry"));
                }
                MenuAction::Submenu(n) => {
                    if !submenu_names.contains(n.as_str()) {
                        errors.push(format!(
                            "{group}: submenu target {n:?} has no [[submenus.{n}]] group"
                        ));
                    }
                    refs.push(n.clone());
                }
                MenuAction::Board(n) if !board_names.contains(n) => {
                    // A warning, not an error: boards are created by admins at
                    // runtime and this may run before the first server seed, so
                    // a missing board isn't necessarily a config mistake.
                    warnings.push(format!("{group}: board target {n:?} does not exist yet"));
                }
                _ => {}
            }
            if let Some(k) = menu_effective_key(e, &action) {
                match keys.get(&k) {
                    Some(prev) => errors.push(format!(
                        "{group}: hotkey {k:?} is bound to both {prev:?} and {:?}",
                        e.action
                    )),
                    None => {
                        keys.insert(k, e.action.clone());
                    }
                }
            }
        }
        refs
    };

    // Reference graph: main menu + every defined submenu.
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    graph.insert(
        "menu".to_string(),
        validate_group("menu", &settings.menu, &mut errors, &mut warnings),
    );
    for (name, entries) in &settings.submenus {
        let group = format!("submenu {name:?}");
        let refs = validate_group(&group, entries, &mut errors, &mut warnings);
        graph.insert(name.clone(), refs);
    }

    // Reachability from the main menu (for unreachable + cycle detection).
    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack = graph.get("menu").cloned().unwrap_or_default();
    while let Some(n) = stack.pop() {
        if reachable.insert(n.clone())
            && let Some(refs) = graph.get(&n)
        {
            stack.extend(refs.iter().cloned());
        }
    }
    for name in settings.submenus.keys() {
        if !reachable.contains(name) {
            warnings.push(format!(
                "submenu {name:?} is never referenced by any submenu: action"
            ));
        }
    }

    // Cycle detection over the submenu subgraph (a cycle would loop forever if
    // the depth cap didn't stop it; flag it either way).
    let mut color: HashMap<&str, u8> = HashMap::new(); // 0=unseen 1=in-progress 2=done
    fn dfs<'a>(
        node: &'a str,
        graph: &'a std::collections::HashMap<String, Vec<String>>,
        color: &mut std::collections::HashMap<&'a str, u8>,
        errors: &mut Vec<String>,
    ) {
        color.insert(node, 1);
        if let Some(refs) = graph.get(node) {
            for r in refs {
                match color.get(r.as_str()).copied().unwrap_or(0) {
                    1 => errors.push(format!("submenu cycle through {r:?}")),
                    0 => dfs(r, graph, color, errors),
                    _ => {}
                }
            }
        }
        color.insert(node, 2);
    }
    // Start from each submenu name present in the graph.
    let submenu_keys: Vec<String> = settings.submenus.keys().cloned().collect();
    for name in &submenu_keys {
        if color.get(name.as_str()).copied().unwrap_or(0) == 0 {
            dfs(name, &graph, &mut color, &mut errors);
        }
    }

    for w in &warnings {
        println!("warning: {w}");
    }
    for e in &errors {
        println!("error: {e}");
    }
    if errors.is_empty() {
        if settings.menu.is_empty() {
            println!("no [[menu]] configured — the built-in default menu is used.");
        } else {
            println!(
                "menu OK: {} top-level entries, {} submenu(s), no errors.",
                settings.menu.len(),
                settings.submenus.len()
            );
        }
    }
    Ok(errors.is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let settings = cli.load_settings()?;
    let pool = db::connect(&cli.resolve_database_url(&settings)).await?;

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
                let status = if u.is_banned() {
                    "banned"
                } else if !u.is_validated() {
                    "pending"
                } else {
                    "ok"
                };
                println!(
                    "{:<20} {:<8} {:<8} {}",
                    u.username,
                    u.role,
                    status,
                    fmt_time(u.created_at)
                );
            }
        }
        Cmd::Pending => {
            let users = admin::pending_users(&pool).await?;
            if users.is_empty() {
                println!("no accounts pending approval");
            } else {
                println!("{:<20} REGISTERED", "USERNAME");
                for u in users {
                    println!("{:<20} {}", u.username, fmt_time(u.created_at));
                }
            }
        }
        Cmd::Approve { username } => {
            if admin::validate_user(&pool, &username).await? {
                audit::log(&pool, audit::BBSCTL, "approve_user", &username, None).await;
                println!("approved '{username}'");
            } else {
                println!("'{username}' is not pending (already active or missing)");
            }
        }
        Cmd::Reject { username } => {
            if admin::reject_user(&pool, &username).await? {
                audit::log(&pool, audit::BBSCTL, "reject_user", &username, None).await;
                println!("rejected and removed '{username}'");
            } else {
                println!("'{username}' is not pending (already active or missing)");
            }
        }
        Cmd::Ban { username } => {
            admin::ban_user(&pool, &username).await?;
            audit::log(&pool, audit::BBSCTL, "ban_user", &username, None).await;
            println!("banned user '{username}'");
        }
        Cmd::Unban { username } => {
            admin::unban_user(&pool, &username).await?;
            audit::log(&pool, audit::BBSCTL, "unban_user", &username, None).await;
            println!("unbanned user '{username}'");
        }
        Cmd::BanIp { ip, reason } => {
            // Manual bans are permanent (no expiry).
            admin::ban_ip(&pool, &ip, &reason, None).await?;
            let detail = (!reason.is_empty()).then_some(reason.as_str());
            audit::log(&pool, audit::BBSCTL, "ban_ip", &ip, detail).await;
            println!("banned ip '{ip}'");
        }
        Cmd::UnbanIp { ip } => {
            admin::unban_ip(&pool, &ip).await?;
            audit::log(&pool, audit::BBSCTL, "unban_ip", &ip, None).await;
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
        Cmd::Broadcast { message } => {
            let msg = message.trim();
            if msg.is_empty() {
                anyhow::bail!("broadcast message is empty");
            }
            let id = admin::queue_broadcast(&pool, msg).await?;
            audit::log(&pool, audit::BBSCTL, "broadcast", "all sessions", Some(msg)).await;
            println!(
                "queued broadcast #{id}; the running server will deliver it to live sessions \
                 within one sweep interval"
            );
        }
        Cmd::Role { username, role } => {
            admin::set_role(&pool, &username, &role).await?;
            audit::log(&pool, audit::BBSCTL, "set_role", &username, Some(&role)).await;
            println!("set role of '{username}' to '{role}'");
        }
        Cmd::Passwd {
            username,
            password,
            no_force,
        } => {
            // A generated password is printed; a supplied one is not echoed
            // back, since it's already in the operator's shell history and
            // repeating it only widens the exposure.
            let generated = password.is_none();
            let new = match password {
                Some(p) => p,
                None => auth::generate_temp_password()?,
            };
            if auth::set_password(&pool, &username, &new, !no_force).await? {
                audit::log(&pool, audit::BBSCTL, "reset_password", &username, None).await;
                if generated {
                    println!("temporary password for '{username}': {new}");
                } else {
                    println!("password set for '{username}'");
                }
                if no_force {
                    println!("(no forced change on next login)");
                } else {
                    println!("they must choose a new password at next login");
                }
            } else {
                println!("no such local user: '{username}'");
            }
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
        Cmd::Polls { limit } => {
            let list = polls::list_polls(&pool, limit).await?;
            println!(
                "{:<5} {:<6} {:<6} {:<14} QUESTION",
                "ID", "VOTES", "STATE", "AUTHOR"
            );
            for p in list {
                println!(
                    "{:<5} {:<6} {:<6} {:<14} {}",
                    p.id,
                    p.total_votes,
                    if p.is_closed() { "closed" } else { "open" },
                    p.author_name,
                    p.question,
                );
            }
        }
        Cmd::ClosePoll { id } => {
            if polls::close_poll(&pool, id, bbs_rs::util::now_unix()).await? {
                println!("closed poll #{id}");
            } else {
                println!("poll #{id} not found or already closed");
            }
        }
        Cmd::RmPoll { id } => {
            if polls::delete_poll(&pool, id).await? {
                println!("removed poll #{id}");
            } else {
                println!("no poll #{id}");
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
                    for (domain, reason, severity) in rows {
                        let tag = if kind == "block" {
                            format!("[{severity}] ")
                        } else {
                            String::new()
                        };
                        println!("  {domain:<30} {tag}{reason}");
                    }
                }
            }
        }
        Cmd::ApAllow { domain, reason } => {
            bbs_rs::services::federation::policy::set(&pool, &domain, "allow", &reason, "suspend")
                .await?;
            println!("allowed {domain}");
        }
        Cmd::ApBlock {
            domain,
            reason,
            severity,
        } => {
            bbs_rs::services::federation::policy::set(&pool, &domain, "block", &reason, &severity)
                .await?;
            println!("blocked {domain} ({severity})");
            if severity == "suspend" {
                println!(
                    "note: this stops what arrives next; it does not delete what already \
                     arrived — use `ap-purge {domain} --yes` for that"
                );
            }
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
        Cmd::ApFollow { user, handle } => {
            let local = local_federatable_user(&pool, &user).await?;
            let remote =
                bbs_rs::web::ap_object::follow_handle(&pool, &settings.federation, &local, &handle)
                    .await?;
            println!(
                "{user} → follow request sent to {remote}; it stays pending until they Accept"
            );
        }
        Cmd::ApUnfollow { user, handle } => {
            let local = local_federatable_user(&pool, &user).await?;
            let removed = bbs_rs::web::ap_object::unfollow_handle(
                &pool,
                &settings.federation,
                &local,
                &handle,
            )
            .await?;
            println!(
                "{}",
                if removed {
                    format!("{user} unfollowed {handle}")
                } else {
                    format!("{user} was not following {handle}")
                }
            );
        }
        Cmd::ApFollowing { user } => {
            use bbs_rs::services::federation::{Origin, follows};
            let local = local_federatable_user(&pool, &user).await?;
            let origin = Origin::from_config(&settings.federation)?;
            let uri = origin.person(&local.username);
            let rows = follows::following(&pool, &uri).await?;
            if rows.is_empty() {
                println!("{user} follows no remote accounts");
            } else {
                for (object, state) in rows {
                    println!("  {state:<9} {object}");
                }
            }
        }
        Cmd::ApBoardPosts { board, limit } => {
            use bbs_rs::services::federation::mirror;
            // Accept a Group actor URI directly, or resolve a `slug@host` handle
            // through the actor row we stored when the board was followed.
            let group_uri = if board.contains("://") {
                board.clone()
            } else {
                sqlx::query_scalar::<_, String>(
                    "SELECT actor_uri FROM users WHERE username = ? AND actor_uri IS NOT NULL",
                )
                .bind(&board)
                .fetch_optional(&pool)
                .await?
                .with_context(|| format!("no followed board {board:?} — follow it first"))?
            };
            // Threaded, like the in-BBS screen (#139) — a flat list of a
            // conversation misrepresents it.
            let posts = mirror::thread(&pool, &group_uri, limit).await?;
            if posts.is_empty() {
                println!("no mirrored posts from {board}");
            } else {
                for item in posts {
                    let p = item.post;
                    let pad = "  ".repeat(item.depth as usize);
                    let lead = if item.depth > 0 { "↳ " } else { "" };
                    println!(
                        "  {pad}{}  {}{} — {}\n      {pad}{}",
                        fmt_time(p.published),
                        lead,
                        p.author_handle,
                        p.subject,
                        p.content.replace('\n', &format!("\n      {pad}"))
                    );
                }
            }
        }
        Cmd::ApReports { all, limit } => {
            use bbs_rs::services::federation::moderation;
            let reports = moderation::reports(&pool, all, limit).await?;
            if reports.is_empty() {
                println!("no {}reports", if all { "" } else { "open " });
            } else {
                for r in reports {
                    let state = if r.resolved_at.is_some() {
                        "resolved"
                    } else {
                        "OPEN"
                    };
                    println!(
                        "#{:<4} {:<8} {}  from {}",
                        r.id,
                        state,
                        fmt_time(r.created_at),
                        r.reporter_handle
                    );
                    if !r.content.is_empty() {
                        println!("      {}", r.content.replace('\n', "\n      "));
                    }
                    for o in r.objects.lines() {
                        println!("      · {o}");
                    }
                }
            }
        }
        Cmd::ApResolve { id } => {
            let done = bbs_rs::services::federation::moderation::resolve_report(&pool, id).await?;
            println!(
                "{}",
                if done {
                    format!("report #{id} marked resolved")
                } else {
                    format!("no open report #{id}")
                }
            );
        }
        Cmd::ApPurge { domain, yes } => {
            anyhow::ensure!(
                yes,
                "refusing to delete content without --yes (this cannot be undone)"
            );
            let p = bbs_rs::services::federation::moderation::purge_domain(&pool, &domain).await?;
            println!(
                "purged {domain}: {} board post(s), {} status(es), {} mirrored post(s), {} mail",
                p.board_posts, p.statuses, p.mirrored_posts, p.mail
            );
        }
        Cmd::Audit { limit } => {
            let entries = audit::recent(&pool, limit).await?;
            println!(
                "{:<20} {:<14} {:<14} {:<24} DETAIL",
                "WHEN", "ACTOR", "ACTION", "TARGET"
            );
            for e in entries {
                println!(
                    "{:<20} {:<14} {:<14} {:<24} {}",
                    fmt_time(e.created_at),
                    e.actor,
                    e.action,
                    e.target,
                    e.detail.as_deref().unwrap_or("-")
                );
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
        Cmd::ValidateMenu => {
            let ok = validate_menu(&settings, &pool).await?;
            if !ok {
                std::process::exit(1);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_with_config(path: &std::path::Path) -> Cli {
        Cli::parse_from(["bbsctl", "--config", &path.to_string_lossy(), "users"])
    }

    /// A config file that exists but doesn't parse must be a hard error — never
    /// a silent fall back to defaults, which would point the command at the
    /// default database instead of the operator's.
    #[test]
    fn malformed_config_is_an_error() {
        let path = std::env::temp_dir().join("bbsctl_malformed_config_test.toml");
        std::fs::write(&path, "this is not = = toml [[[\n").unwrap();

        let err = cli_with_config(&path).load_settings().unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("parsing config"), "unexpected error: {msg}");
        assert!(msg.contains(&*path.to_string_lossy()), "no path in: {msg}");

        std::fs::remove_file(&path).ok();
    }

    /// A duplicate key is the realistic trigger, and toml rejects it too.
    #[test]
    fn duplicate_key_is_an_error() {
        let path = std::env::temp_dir().join("bbsctl_duplicate_key_test.toml");
        std::fs::write(
            &path,
            "[federation]\nallowlist_only = true\nallowlist_only = false\n",
        )
        .unwrap();

        assert!(cli_with_config(&path).load_settings().is_err());

        std::fs::remove_file(&path).ok();
    }

    /// A missing file still yields defaults: bbsctl may run before the server
    /// has written its config, and that's not an operator error.
    #[test]
    fn missing_config_falls_back_to_defaults() {
        let path = std::env::temp_dir().join("bbsctl_definitely_missing_config.toml");
        std::fs::remove_file(&path).ok();

        let settings = cli_with_config(&path).load_settings().unwrap();
        assert_eq!(
            settings.network.database_url,
            Settings::default().network.database_url
        );
    }
}
