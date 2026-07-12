//! Configuration: a TOML file (`bbs.toml` by default) parsed into [`Settings`],
//! with CLI flags overriding individual values. Precedence is
//! **defaults < file < CLI**. A commented default file is written on first run.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Serialize};

/// Command-line arguments: the config path plus optional per-field overrides.
/// Overrides are `Option` so an unset flag never clobbers the file value.
#[derive(Parser, Debug, Clone)]
#[command(name = "bbs-rs", about = "A bulletin board system served over SSH")]
pub struct Cli {
    /// Path to the TOML config file (created with defaults if missing).
    #[arg(long, default_value = "bbs.toml")]
    pub config: PathBuf,

    /// Override the bind address.
    #[arg(long)]
    pub host: Option<String>,

    /// Override the SSH port.
    #[arg(long)]
    pub port: Option<u16>,

    /// Override the SQLite database URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Override the SSH host-key path.
    #[arg(long)]
    pub host_key: Option<PathBuf>,

    /// Apply pending database migrations and exit, without starting the server.
    #[arg(long)]
    pub migrate: bool,
}

/// Full runtime configuration. Every section is `#[serde(default)]`, so a
/// partial file (or none at all) still yields a complete, valid config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub bbs: Bbs,
    pub network: Network,
    pub features: Features,
    pub abuse: Abuse,
    pub accounts: Accounts,
}

/// Branding shown to connected users.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Bbs {
    /// Board name (title bar, startup log, help heading).
    pub name: String,
    /// Short subtitle shown on the main menu and help screen.
    pub tagline: String,
    /// Sysop name shown in the help footer (blank to hide).
    pub sysop: String,
    /// Message-of-the-day banner shown on the main menu (blank to hide).
    pub welcome: String,
}

/// Network and SSH tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub host_key: PathBuf,
    /// Disconnect idle sessions after this many seconds.
    pub inactivity_timeout_secs: u64,
    /// Delay before an SSH auth rejection is returned.
    pub auth_rejection_time_secs: u64,
    /// How often to sweep for banned users/IPs and kick live sessions.
    pub ban_sweep_interval_secs: u64,
    /// Fallback terminal size when a client reports 0x0.
    pub default_cols: u16,
    pub default_rows: u16,
}

/// Feature toggles an operator can turn off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    pub registration: bool,
    pub guest: bool,
    pub private_mail: bool,
    pub who_online: bool,
}

/// Abuse protection: auto-ban IPs with repeated failed logins.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Abuse {
    /// Auto-ban an IP after this many failed logins within the window. 0 disables.
    pub max_failures: u32,
    /// Sliding window for counting failures, in seconds.
    pub window_secs: i64,
    /// How long an auto-ban lasts, in seconds. 0 = permanent.
    pub ban_secs: i64,
}

impl Abuse {
    /// Whether auto-ban is active.
    pub fn enabled(&self) -> bool {
        self.max_failures > 0
    }
}

/// Account-registration policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Accounts {
    /// Usernames that may not be registered. Matching is case-insensitive and
    /// ignores surrounding whitespace. `guest` is always reserved regardless of
    /// this list (it is the shared limited account).
    pub reserved_usernames: Vec<String>,
}

impl Default for Accounts {
    fn default() -> Self {
        Self {
            reserved_usernames: vec!["root".into(), "admin".into()],
        }
    }
}

impl Accounts {
    /// Whether `name` may not be used for a new account. Comparison is
    /// case-insensitive and trims surrounding whitespace; `guest` is always
    /// reserved.
    pub fn is_reserved(&self, name: &str) -> bool {
        let name = name.trim();
        name.eq_ignore_ascii_case("guest")
            || self
                .reserved_usernames
                .iter()
                .any(|r| r.trim().eq_ignore_ascii_case(name))
    }
}

impl Default for Bbs {
    fn default() -> Self {
        Self {
            name: "bbs-rs".into(),
            tagline: "a tiny bulletin board over SSH".into(),
            sysop: String::new(),
            welcome: "Welcome to the board.".into(),
        }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 2222,
            database_url: "sqlite://bbs.db?mode=rwc".into(),
            host_key: PathBuf::from("host_key"),
            inactivity_timeout_secs: 3600,
            auth_rejection_time_secs: 2,
            ban_sweep_interval_secs: 10,
            default_cols: 80,
            default_rows: 24,
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Self {
            registration: true,
            guest: true,
            private_mail: true,
            who_online: true,
        }
    }
}

impl Default for Abuse {
    fn default() -> Self {
        // Enabled by default with a conservative policy: 10 failures in 10
        // minutes → a 1-hour ban.
        Self {
            max_failures: 10,
            window_secs: 600,
            ban_secs: 3600,
        }
    }
}

impl Network {
    pub fn inactivity_timeout(&self) -> Duration {
        Duration::from_secs(self.inactivity_timeout_secs)
    }
    pub fn auth_rejection_time(&self) -> Duration {
        Duration::from_secs(self.auth_rejection_time_secs)
    }
    pub fn ban_sweep_interval(&self) -> Duration {
        Duration::from_secs(self.ban_sweep_interval_secs)
    }
}

impl Settings {
    /// Load settings: create the file with defaults if missing, parse it, then
    /// apply CLI overrides.
    pub fn load(cli: &Cli) -> anyhow::Result<Settings> {
        let path = &cli.config;
        if !path.exists() {
            std::fs::write(path, DEFAULT_CONFIG_TOML)
                .with_context(|| format!("writing default config to {}", path.display()))?;
            tracing::info!("wrote default config to {}", path.display());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut settings: Settings =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;

        // CLI overrides win over the file.
        if let Some(host) = &cli.host {
            settings.network.host = host.clone();
        }
        if let Some(port) = cli.port {
            settings.network.port = port;
        }
        if let Some(url) = &cli.database_url {
            settings.network.database_url = url.clone();
        }
        if let Some(key) = &cli.host_key {
            settings.network.host_key = key.clone();
        }
        Ok(settings)
    }
}

/// The default config written on first run. Kept in sync with the `Default`
/// impls above; this commented form is preferred over serializing `Default`
/// (which would drop the comments).
pub const DEFAULT_CONFIG_TOML: &str = "\
# bbs-rs configuration.
# CLI flags (--host, --port, --database-url, --host-key) override these values.

[bbs]
# Board name — shown in the title bar, startup log, and help.
name = \"bbs-rs\"
# Short subtitle shown on the main menu and help screen.
tagline = \"a tiny bulletin board over SSH\"
# Sysop name shown in the help footer (blank to hide).
sysop = \"\"
# Message-of-the-day banner shown on the main menu (blank to hide).
welcome = \"Welcome to the board.\"

[network]
host = \"0.0.0.0\"
port = 2222
database_url = \"sqlite://bbs.db?mode=rwc\"
host_key = \"host_key\"
# Disconnect idle sessions after this many seconds.
inactivity_timeout_secs = 3600
# Delay before an SSH auth rejection is returned.
auth_rejection_time_secs = 2
# How often to sweep for banned users/IPs and kick their live sessions.
ban_sweep_interval_secs = 10
# Fallback terminal size when a client reports 0x0.
default_cols = 80
default_rows = 24

[features]
# Allow new-account registration (from the guest session).
registration = true
# Allow the shared guest account to log in.
guest = true
# Enable private user-to-user mail.
private_mail = true
# Enable the who's-online view.
who_online = true

[abuse]
# Auto-ban an IP after this many failed logins within the window. 0 disables.
max_failures = 10
# Sliding window for counting failures, in seconds.
window_secs = 600
# How long an auto-ban lasts, in seconds. 0 = permanent.
ban_secs = 3600

[accounts]
# Usernames that may not be registered (case-insensitive; whitespace-trimmed).
# \"guest\" is always reserved regardless of this list.
reserved_usernames = [\"root\", \"admin\"]
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_parses_and_matches_defaults() {
        let parsed: Settings = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        let def = Settings::default();
        assert_eq!(parsed.bbs.name, def.bbs.name);
        assert_eq!(parsed.network.port, def.network.port);
        assert_eq!(
            parsed.network.ban_sweep_interval_secs,
            def.network.ban_sweep_interval_secs
        );
        assert_eq!(parsed.features.registration, def.features.registration);
        assert_eq!(
            parsed.accounts.reserved_usernames,
            def.accounts.reserved_usernames
        );
    }

    #[test]
    fn partial_file_fills_from_defaults() {
        let partial = "[bbs]\nname = \"MyBoard\"\n";
        let s: Settings = toml::from_str(partial).unwrap();
        assert_eq!(s.bbs.name, "MyBoard");
        // Unspecified fields fall back to defaults.
        assert_eq!(s.network.port, 2222);
        assert!(s.features.guest);
        // The reserved-username list defaults to root + admin.
        assert_eq!(s.accounts.reserved_usernames, vec!["root", "admin"]);
    }

    #[test]
    fn reserved_usernames_are_matched_case_insensitively() {
        let a = Accounts::default();
        assert!(a.is_reserved("root"));
        assert!(a.is_reserved("Admin"));
        assert!(a.is_reserved("  ROOT  "));
        // guest is always reserved even when absent from the list.
        assert!(a.is_reserved("guest"));
        assert!(a.is_reserved("GUEST"));
        assert!(!a.is_reserved("alice"));
    }

    #[test]
    fn empty_list_still_reserves_guest() {
        let a = Accounts {
            reserved_usernames: vec![],
        };
        assert!(a.is_reserved("guest"));
        assert!(!a.is_reserved("root"));
    }
}
