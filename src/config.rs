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
    pub limits: Limits,
    pub files: Files,
    pub theme: ThemeConfig,
    pub art: Art,
    pub web: Web,
}

/// Optional browser frontend: a WebSocket + xterm.js terminal that reuses the
/// whole TUI. Disabled by default; enable and pick a bind address to serve it
/// alongside SSH.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Web {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".into(),
            port: 8080,
        }
    }
}

/// Operator-customizable color theme. `preset` names a built-in base
/// ("classic", "mono", "amber", "matrix"); any other field, when set, overrides
/// that one color. Colors are strings: a named color (`cyan`, `darkgray`, …),
/// a 256-palette index (`"200"`), or a hex triple (`"#ff8800"`). Resolution to
/// concrete colors lives in [`crate::app::theme`]; this struct is just the
/// raw (all-optional) config so omitted fields inherit from the preset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: Option<String>,
    pub title_fg: Option<String>,
    pub title_bg: Option<String>,
    pub accent: Option<String>,
    pub highlight: Option<String>,
    pub warning_fg: Option<String>,
    pub warning_bg: Option<String>,
    pub dim: Option<String>,
}

/// Operator-supplied ANSI/text art. Files live under `dir`; `welcome` is shown
/// on the main menu, and `screens` maps a screen key (e.g. `board_list`,
/// `file_areas`) to a file rendered as a header band on that screen. Real
/// CP437 `.ans` art and UTF-8 text with ANSI color escapes are both supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Art {
    pub dir: PathBuf,
    pub welcome: String,
    pub screens: std::collections::HashMap<String, String>,
}

impl Default for Art {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("art"),
            welcome: String::new(),
            screens: std::collections::HashMap::new(),
        }
    }
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
    /// Public hostname clients use to reach the BBS, shown in connect
    /// instructions (e.g. the SFTP download hint). Blank falls back to `host`,
    /// or `localhost` when `host` is a wildcard bind address.
    pub hostname: String,
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
    pub oneliners: bool,
    pub pubkey_auth: bool,
    pub file_areas: bool,
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

/// Per-user rate limits (post/mail/oneliner throttling). Counts a user's own
/// rows created within `window_secs`; a `0` cap disables that limit. Admins are
/// never throttled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Sliding window for counting a user's recent actions, in seconds.
    pub window_secs: i64,
    /// Max board posts per user per window (0 disables).
    pub max_posts: u32,
    /// Max mail sent per user per window (0 disables).
    pub max_mail: u32,
    /// Max oneliners per user per window (0 disables).
    pub max_oneliners: u32,
}

impl Default for Limits {
    fn default() -> Self {
        // A generous default: enough for normal use, tight enough to blunt
        // scripted spam. Pairs with the [abuse] auto-ban guard.
        Self {
            window_secs: 60,
            max_posts: 5,
            max_mail: 10,
            max_oneliners: 8,
        }
    }
}

impl Limits {
    /// The start of the current window (Unix seconds), or `None` if the window
    /// is disabled (`window_secs <= 0`).
    pub fn window_start(&self, now: i64) -> Option<i64> {
        (self.window_secs > 0).then(|| now - self.window_secs)
    }
}

/// File-area storage policy: where uploaded files live, plus per-file and
/// per-user limits and an optional extension allowlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Files {
    /// Directory (relative to the working dir, or absolute) holding file blobs.
    pub storage_dir: PathBuf,
    /// Maximum size of a single file, in bytes. 0 = unlimited.
    pub max_file_bytes: u64,
    /// Maximum total bytes one user may store across all areas. 0 = unlimited.
    pub user_quota_bytes: u64,
    /// Allowed file extensions (lowercase, no dot), e.g. ["txt", "zip"]. An
    /// empty list allows any extension.
    pub allowed_extensions: Vec<String>,
    /// Maximum bytes to read/decompress when previewing a file or archive entry
    /// in the BBS (guards against huge files / zip bombs).
    pub max_preview_bytes: u64,
    /// Maximum number of entries to list from an archive.
    pub max_archive_entries: usize,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            storage_dir: PathBuf::from("files"),
            max_file_bytes: 10 * 1024 * 1024,    // 10 MiB
            user_quota_bytes: 100 * 1024 * 1024, // 100 MiB
            allowed_extensions: Vec::new(),
            max_preview_bytes: 256 * 1024, // 256 KiB
            max_archive_entries: 1000,
        }
    }
}

impl Files {
    /// Whether `filename`'s extension is permitted. An empty allowlist permits
    /// everything; the check is case-insensitive.
    pub fn extension_allowed(&self, filename: &str) -> bool {
        if self.allowed_extensions.is_empty() {
            return true;
        }
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        self.allowed_extensions
            .iter()
            .any(|allowed| allowed.trim_start_matches('.').eq_ignore_ascii_case(&ext))
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
            hostname: String::new(),
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
            oneliners: true,
            pubkey_auth: true,
            file_areas: true,
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

    /// The hostname to show clients in connect instructions: the configured
    /// public `hostname` if set, otherwise `host` — mapping a wildcard bind
    /// address (`0.0.0.0` / `::` / empty) to `localhost`, which is at least
    /// connectable.
    pub fn connect_host(&self) -> String {
        let h = self.hostname.trim();
        if !h.is_empty() {
            return h.to_string();
        }
        match self.host.trim() {
            "" | "0.0.0.0" | "::" | "[::]" => "localhost".to_string(),
            other => other.to_string(),
        }
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
# Public hostname clients use to reach the BBS, shown in connect instructions
# (e.g. the SFTP download hint). Blank falls back to host, or localhost when
# host is a wildcard address.
hostname = \"\"
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
# Enable the oneliners / graffiti wall.
oneliners = true
# Allow SSH public-key authentication (users register keys in the BBS).
pubkey_auth = true
# Enable file areas (browse downloadable files).
file_areas = true

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

[limits]
# Per-user rate limits (admins are never throttled). A 0 cap disables that
# limit. Counts the user's own rows created within the window.
window_secs = 60
# Max board posts per user per window.
max_posts = 5
# Max mail sent per user per window.
max_mail = 10
# Max oneliners per user per window.
max_oneliners = 8

[files]
# Where uploaded file blobs are stored (relative to the working dir).
storage_dir = \"files\"
# Maximum size of a single file, in bytes (0 = unlimited). Default 10 MiB.
max_file_bytes = 10485760
# Maximum total bytes one user may store (0 = unlimited). Default 100 MiB.
user_quota_bytes = 104857600
# Allowed file extensions (lowercase, no dot); empty allows any, e.g.
# allowed_extensions = [\"txt\", \"zip\", \"png\"]
allowed_extensions = []
# Max bytes read/decompressed when previewing a file or archive entry in the BBS.
max_preview_bytes = 262144
# Max entries listed from an archive.
max_archive_entries = 1000

[theme]
# Color theme. `preset` picks a built-in base: classic (default), mono, amber,
# or matrix. Uncomment any field below to override just that color. Colors are
# a name (black, red, green, yellow, blue, magenta, cyan, gray, darkgray, the
# light* variants, white, reset), a 256-palette index (\"208\"), or hex
# (\"#ff8800\").
preset = \"classic\"
# title_fg = \"black\"     # title-bar text
# title_bg = \"cyan\"      # title-bar background
# accent = \"cyan\"        # headings, tags, author names
# highlight = \"green\"    # \"new\"/unread markers
# warning_fg = \"black\"   # status/warning text
# warning_bg = \"yellow\"  # status/warning background
# dim = \"darkgray\"       # secondary text, hints, labels

[art]
# ANSI/text art. Files live under `dir` (relative to the working dir). Real
# CP437 .ans art and UTF-8 text with ANSI color escapes both work.
dir = \"art\"
# Shown on the main menu (a file name under `dir`; blank = none).
welcome = \"\"
# Optional per-screen header art: map a screen key to a file under `dir`.
# Keys: main_menu, bulletins, board_list, message_list, mailbox, who_online,
# profile, stats, search, file_areas, file_list, keys, help, admin.
# [art.screens]
# board_list = \"boards.ans\"
# file_areas = \"files.ans\"

[web]
# Browser frontend: a WebSocket + xterm.js terminal that reuses the whole TUI,
# served alongside SSH. Off by default.
enabled = false
host = \"0.0.0.0\"
port = 8080
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
        assert_eq!(parsed.features.oneliners, def.features.oneliners);
        assert_eq!(parsed.features.pubkey_auth, def.features.pubkey_auth);
        assert_eq!(parsed.limits.max_posts, def.limits.max_posts);
        assert_eq!(parsed.files.storage_dir, def.files.storage_dir);
        assert_eq!(parsed.files.max_file_bytes, def.files.max_file_bytes);
        assert_eq!(parsed.limits.window_secs, def.limits.window_secs);
        assert_eq!(
            parsed.accounts.reserved_usernames,
            def.accounts.reserved_usernames
        );
        // Theme/art sections parse; the template names the classic preset and
        // the default art dir.
        assert_eq!(parsed.theme.preset.as_deref(), Some("classic"));
        assert_eq!(parsed.art.dir, def.art.dir);
        assert!(parsed.art.welcome.is_empty());
        assert_eq!(parsed.web.enabled, def.web.enabled);
        assert_eq!(parsed.web.port, def.web.port);
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
    fn connect_host_prefers_hostname_then_maps_wildcards() {
        let mut net = Network::default(); // host 0.0.0.0, no hostname
        assert_eq!(net.connect_host(), "localhost");

        net.host = "bbs.example.com".into();
        assert_eq!(net.connect_host(), "bbs.example.com");

        net.hostname = "public.example.net".into();
        assert_eq!(net.connect_host(), "public.example.net");

        net.hostname = "  ".into(); // blank falls back to host
        assert_eq!(net.connect_host(), "bbs.example.com");
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
