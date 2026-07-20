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
    pub finger: Finger,
    pub federation: Federation,
    pub oneliners: Oneliners,
    pub seed: Seed,
    /// External "door" programs launchable per session (classic BBS doors).
    #[serde(default)]
    pub doors: Vec<Door>,
}

/// An external program launchable from the Doors menu. Run on a pseudo-terminal
/// with the session's user info in the environment (and an optional drop file),
/// with an optional wall-clock time limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Door {
    /// Menu label.
    pub name: String,
    /// Program to run.
    pub command: String,
    /// Arguments passed to the program.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (also where a drop file is written). Defaults to the
    /// current directory.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Kill the program after this many seconds (0 = no limit).
    #[serde(default)]
    pub time_limit_secs: u64,
    /// Write a classic drop file before launch: `door.sys` or `dorinfo1.def`
    /// (case-insensitive). Blank/unset writes none.
    #[serde(default)]
    pub drop_file: Option<String>,
}

/// First-run seeded content: the boards created when the board table is empty,
/// and the shared guest account's password. Both are optional — unset uses the
/// built-in defaults; see [`Seed::boards`] / [`Seed::guest_password`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seed {
    /// Boards to create on first run. `None` (unset) uses the built-in defaults
    /// (General + Announcements); `Some([])` seeds no boards.
    pub boards: Option<Vec<SeedBoard>>,
    /// Password for the shared `guest` account (unset → "guest").
    pub guest_password: Option<String>,
}

/// One operator-defined seed board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeedBoard {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Minimum role to read (`guest` | `user` | `admin`). Default `guest`.
    #[serde(default = "default_read_role")]
    pub min_read: String,
    /// Minimum role to post. Default `user`.
    #[serde(default = "default_write_role")]
    pub min_write: String,
}

fn default_read_role() -> String {
    "guest".into()
}
fn default_write_role() -> String {
    "user".into()
}

impl Seed {
    /// The boards to seed: the operator's list, or the built-in defaults when
    /// unset.
    pub fn boards(&self) -> Vec<SeedBoard> {
        self.boards.clone().unwrap_or_else(default_seed_boards)
    }

    /// The guest account's password (configured, or "guest").
    pub fn guest_password(&self) -> &str {
        self.guest_password.as_deref().unwrap_or("guest")
    }
}

/// The built-in default boards, used when `[seed] boards` is unset.
fn default_seed_boards() -> Vec<SeedBoard> {
    vec![
        SeedBoard {
            name: "General".into(),
            description: "General chatter and introductions".into(),
            min_read: "guest".into(),
            min_write: "user".into(),
        },
        SeedBoard {
            name: "Announcements".into(),
            description: "System news and updates".into(),
            min_read: "guest".into(),
            min_write: "admin".into(),
        },
    ]
}

/// Oneliners (graffiti wall) policy. Separate from the `[features] oneliners`
/// on/off toggle.
///
/// The old `max_entries` ring buffer is gone (#108): oneliners are now
/// ActivityPub statuses, and a federated post has a permanent URI — trimming
/// one out from under remote servers would orphan their references. The wall
/// grows without bound; the rate limit and moderation replace the trim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Oneliners {
    /// Max characters in a oneliner body (0 = no length cap). Defaults to
    /// [`crate::services::oneliners::MAX_LEN`] (500, matching Mastodon).
    pub max_length: usize,
}

impl Default for Oneliners {
    fn default() -> Self {
        Self {
            max_length: crate::services::oneliners::MAX_LEN,
        }
    }
}

/// Optional browser frontend: a WebSocket + xterm.js terminal that reuses the
/// whole TUI. Disabled by default; enable and pick a bind address to serve it
/// alongside SSH.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Web {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// Public hostname browsers use to reach the frontend, shown in connect
    /// instructions (the mirror of `[network] hostname`). Blank falls back to
    /// the first `acme_domains` entry, then `host` — or `localhost` when `host`
    /// is a wildcard bind address. Set this when the frontend sits behind a
    /// reverse proxy or on a different domain than SSH.
    pub hostname: String,
    /// Serve HTTPS/WSS. On by default: with no cert configured the server
    /// generates a persistent self-signed cert so TLS works out of the box.
    pub tls: bool,
    /// PEM certificate-chain path. Blank = auto-generate a self-signed cert at
    /// `tls_cert`/`tls_key` (created if missing). Set both to bring your own.
    pub tls_cert: String,
    /// PEM private-key path (see `tls_cert`).
    pub tls_key: String,
    /// Domains to fetch a trusted Let's Encrypt cert for (ACME, TLS-ALPN-01).
    /// Non-empty takes precedence over `tls_cert`/`tls_key`. Requires public
    /// DNS and reachability on port 443.
    pub acme_domains: Vec<String>,
    /// ACME account contact email.
    pub acme_email: String,
    /// Directory where ACME caches the account key and issued certs.
    pub acme_cache: String,
    /// Use the Let's Encrypt staging environment (untrusted certs, higher rate
    /// limits) for testing the ACME flow.
    pub acme_staging: bool,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".into(),
            port: 8088,
            hostname: String::new(),
            tls: true,
            tls_cert: String::new(),
            tls_key: String::new(),
            acme_domains: Vec::new(),
            acme_email: String::new(),
            acme_cache: "acme-cache".into(),
            acme_staging: false,
        }
    }
}

/// A read-only `finger` service (RFC 1288, #77). Off by default. When enabled
/// it listens on TCP `port` (79 by convention, which needs privilege to bind —
/// operators typically use a higher port behind a redirect, or grant the
/// capability). `finger @host` lists who's online; `finger user@host` shows a
/// user's public profile. No auth, no writes; a user can opt out per-account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Finger {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for Finger {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".into(),
            port: 79,
        }
    }
}

/// ActivityPub federation (epic #113). Off by default.
///
/// `origin` is deliberately **not** derived from `[web]`. `Web::connect_url()`
/// can legitimately return `https://localhost:8088`, and an ActivityPub `id`
/// URI is a permanent primary key across the whole network — once an actor or
/// post has been delivered to a remote server, its URI can never be rewritten.
/// So the origin is stated explicitly and validated fail-closed at startup: a
/// board that can't federate correctly refuses to federate at all, rather than
/// minting URIs it will be stuck with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Federation {
    pub enabled: bool,
    /// Public origin actor URIs are minted from, e.g. `https://bbs.example.com`.
    /// Scheme + host only — no port, path, or query (see [`Federation::origin`]).
    pub origin: String,
    /// Only federate with domains explicitly allowed in `ap_blocks`. On by
    /// default: open federation means volunteering to moderate the internet.
    pub allowlist_only: bool,
    /// How often the durable outbound delivery queue drains.
    pub delivery_interval_secs: u64,
    /// Give up on an activity after this many failed delivery attempts.
    pub delivery_max_attempts: u32,
    /// Allow addressing private mail to remote fediverse accounts. **Off by
    /// default, and deliberately so:** fediverse DMs are *not* private — they
    /// sit in plaintext on every server they touch. When on, addressing a
    /// `user@host` recipient sends a non-private message off the BBS, which the
    /// compose UI labels as such. Local mail is always private and unaffected.
    pub allow_remote_dms: bool,
    /// **Local testing only.** Permits `http://`, `localhost`, IP literals, and
    /// non-default ports in `origin` so two instances can federate on one
    /// machine. Never enable on a real board: the URIs you mint are permanent.
    pub debug_insecure: bool,
}

impl Default for Federation {
    fn default() -> Self {
        Self {
            enabled: false,
            origin: String::new(),
            allowlist_only: true,
            delivery_interval_secs: 30,
            delivery_max_attempts: 10,
            allow_remote_dms: false,
            debug_insecure: false,
        }
    }
}

impl Federation {
    pub fn delivery_interval(&self) -> Duration {
        Duration::from_secs(self.delivery_interval_secs.max(1))
    }

    /// Validate `origin` and return it normalized (scheme + host, no trailing
    /// slash) — the string every actor/object URI is built from.
    ///
    /// Fail-closed by design. Every rejection here is something that would
    /// either break interop outright or permanently poison our URIs.
    pub fn origin(&self) -> anyhow::Result<String> {
        let raw = self.origin.trim();
        anyhow::ensure!(
            !raw.is_empty(),
            "[federation] origin must be set when federation is enabled — it's the \
             permanent base of every actor URI (e.g. \"https://bbs.example.com\")"
        );
        let url = url::Url::parse(raw)
            .with_context(|| format!("[federation] origin {raw:?} is not a valid URL"))?;

        match url.scheme() {
            "https" => {}
            "http" if self.debug_insecure => {}
            other => anyhow::bail!(
                "[federation] origin must use https (got {other:?}). Remote servers fetch us \
                 with ordinary HTTPS clients, so a plaintext or self-signed origin cannot \
                 interop. Set debug_insecure = true only for local testing."
            ),
        }

        let host = url
            .host()
            .context("[federation] origin must include a host")?;
        if !self.debug_insecure {
            // An IP literal can't be the subject of an `acct:` URI, so WebFinger
            // discovery — how @user@host is resolved — could never work.
            anyhow::ensure!(
                matches!(host, url::Host::Domain(_)),
                "[federation] origin must be a domain name, not an IP address — WebFinger \
                 acct: URIs are built on the DNS name"
            );
            let domain = url.host_str().unwrap_or_default();
            anyhow::ensure!(
                domain != "localhost" && !domain.ends_with(".localhost") && domain.contains('.'),
                "[federation] origin host {domain:?} is not reachable from other servers. \
                 Actor URIs are permanent, so a localhost origin would poison every URI \
                 this board ever mints."
            );
            // `Url::port()` is None for the scheme's default, so this only fires
            // on a genuinely non-standard port.
            if let Some(port) = url.port() {
                anyhow::bail!(
                    "[federation] origin must not include a port (got {port}). RFC 7565 \
                     acct: URIs have no port component, so `acct:user@{}:{port}` is invalid \
                     and WebFinger discovery would fail. Serve on 443 ([web] port = 443 \
                     with acme_domains), or put a reverse proxy in front.",
                    url.host_str().unwrap_or_default()
                );
            }
        }

        anyhow::ensure!(
            url.path() == "/" && url.query().is_none() && url.fragment().is_none(),
            "[federation] origin must be scheme + host only, with no path or query \
             (got {raw:?})"
        );

        Ok(raw.trim_end_matches('/').to_string())
    }
}

impl Web {
    /// The hostname to show clients in connect instructions: the configured
    /// public `hostname` if set, else the first ACME domain (which is by
    /// definition public), else `host` — mapping a wildcard bind address
    /// (`0.0.0.0` / `::` / empty) to `localhost`. Mirrors
    /// [`Network::connect_host`].
    pub fn connect_host(&self) -> String {
        let h = self.hostname.trim();
        if !h.is_empty() {
            return h.to_string();
        }
        if let Some(d) = self.acme_domains.iter().find(|d| !d.trim().is_empty()) {
            return d.trim().to_string();
        }
        match self.host.trim() {
            "" | "0.0.0.0" | "::" | "[::]" => "localhost".to_string(),
            other => other.to_string(),
        }
    }

    /// The URL to show clients for the browser frontend, e.g.
    /// `https://bbs.example.com` or `http://localhost:8088`. The port is
    /// omitted when it's the scheme's default, so a proxied board on 443 reads
    /// cleanly.
    pub fn connect_url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        let default_port = if self.tls { 443 } else { 80 };
        let host = self.connect_host();
        if self.port == default_port {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{}", self.port)
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Tell users about the other way in — show browser users the SSH address
    /// and SSH users the web URL (needs `[network] hostname` / `[web] hostname`
    /// set to be useful). Turn off to keep the other transport unadvertised.
    pub advertise_transports: bool,
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
    /// Max characters in a post/mail subject (0 disables).
    pub max_subject_chars: usize,
    /// Max characters in a post/mail body (0 disables).
    pub max_body_chars: usize,
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
            max_subject_chars: 120,
            max_body_chars: 8000,
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
            advertise_transports: true,
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
# Tell users about the other way in: browser users see the SSH address, SSH
# users see the web URL (when [web] is enabled). Set the hostname fields below
# for these to show a reachable address rather than localhost.
advertise_transports = true

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
# Max characters in a post/mail subject (0 disables).
max_subject_chars = 120
# Max characters in a post/mail body (0 disables).
max_body_chars = 8000

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
port = 8088
# Public hostname browsers use to reach the frontend, shown in connect
# instructions (the mirror of [network] hostname). Blank falls back to the first
# acme_domains entry, then host, or localhost when host is a wildcard address.
# Set this when the frontend is behind a reverse proxy or on its own domain.
hostname = \"\"
# TLS (HTTPS/WSS). On by default: with no cert configured a persistent
# self-signed cert is generated at tls_cert/tls_key (default web-cert.pem /
# web-key.pem), so TLS works out of the box — browsers show a one-time trust
# warning until you install a real cert. Set tls = false for plain HTTP.
tls = true
# Bring your own cert instead (real CA, mkcert, certbot output):
# tls_cert = \"web-cert.pem\"
# tls_key  = \"web-key.pem\"
# Or fetch a trusted Let's Encrypt cert automatically (ACME). Takes precedence
# over tls_cert/tls_key. Requires public DNS and reachability on port 443:
# acme_domains = [\"bbs.example.com\"]
# acme_email   = \"sysop@example.com\"
# acme_cache   = \"acme-cache\"
# acme_staging = false   # true = Let's Encrypt staging (untrusted, for testing)

[finger]
# A read-only finger service (RFC 1288). Off by default. `finger @host` lists
# who's online; `finger user@host` shows a user's public profile. No auth, no
# writes; a user can hide themselves from the Profile screen (press f).
enabled = false
host = \"0.0.0.0\"
# Port 79 is the finger convention but needs privilege to bind — run behind a
# redirect, grant the capability, or pick a high port.
port = 79

[federation]
# ActivityPub federation: syndicate boards across bbs-rs instances, and make
# users user@host — followable from Mastodon. Off by default. See
# docs/FEDERATION.md.
enabled = false
# The public origin every actor URI is minted from, e.g.
# \"https://bbs.example.com\". Scheme + host only: no port, no path.
#
# THIS IS PERMANENT. ActivityPub ids are primary keys across the whole network;
# once delivered to a remote server they can never be rewritten, so changing
# the origin later orphans every remote follow. It is validated fail-closed at
# startup (https, a real domain, no port) — a board that can't federate
# correctly refuses to federate at all.
#
# Note RFC 7565 acct: URIs have no port component, so the frontend must be
# reachable on 443: set [web] port = 443 with acme_domains, or reverse-proxy.
origin = \"\"
# Only federate with domains explicitly allowed (bbsctl). On by default: open
# federation means volunteering to moderate the entire internet.
allowlist_only = true
delivery_interval_secs = 30   # how often the outbound delivery queue drains
delivery_max_attempts = 10    # give up on an activity after this many failures
# Allow private mail to remote fediverse accounts. OFF by default: fediverse DMs
# are NOT private (plaintext on every server they touch). When on, the compose
# UI labels remote mail as leaving the BBS. Local mail is always private.
allow_remote_dms = false
# debug_insecure = false      # LOCAL TESTING ONLY: allows http/localhost/ports

[oneliners]
# Graffiti-wall policy (separate from the [features] oneliners on/off toggle).
# Oneliners are also this board's ActivityPub statuses, so the wall no longer
# auto-trims: a federated post has a permanent URI, and deleting one out from
# under remote servers would orphan their references. Use [limits]
# max_oneliners and bbsctl rm-oneliner to keep the wall in hand.
max_length = 500       # max characters per oneliner (0 = no cap; 500 = Mastodon parity)

[seed]
# First-run seeded content. Boards are created only when the board table is
# empty (i.e. on a fresh database).
# guest_password = \"guest\"   # password for the shared guest account
# Uncomment to define your own boards (replaces the General + Announcements
# defaults). min_read/min_write are guest|user|admin and default to guest/user.
# boards = [
#   { name = \"General\", description = \"General chatter\", min_write = \"user\" },
#   { name = \"Announcements\", description = \"System news\", min_write = \"admin\" },
#   { name = \"Staff\", description = \"Admins only\", min_read = \"admin\", min_write = \"admin\" },
# ]

# Door games / external programs. Each runs on a pseudo-terminal with the user's
# info in the environment (BBS_USER, BBS_TIME_LEFT_SECS, …) and, if requested, a
# drop file (door.sys / dorinfo1.def) in the working dir. A Doors menu appears
# when at least one is configured. Uncomment to add some:
# [[doors]]
# name = \"Adventure\"
# command = \"/usr/games/adventure\"
# args = []
# cwd = \"/var/bbs/doors/adventure\"
# time_limit_secs = 900        # 0 = no limit
# drop_file = \"dorinfo1.def\"   # or \"door.sys\"; blank = none
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
        assert_eq!(
            parsed.limits.max_subject_chars,
            def.limits.max_subject_chars
        );
        assert_eq!(parsed.limits.max_body_chars, def.limits.max_body_chars);
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
        assert_eq!(parsed.web.tls, def.web.tls);
        assert_eq!(parsed.web.acme_cache, def.web.acme_cache);
        assert_eq!(parsed.web.hostname, def.web.hostname);
        assert_eq!(parsed.federation.enabled, def.federation.enabled);
        assert_eq!(parsed.federation.origin, def.federation.origin);
        assert_eq!(
            parsed.federation.allowlist_only,
            def.federation.allowlist_only
        );
        assert_eq!(
            parsed.federation.delivery_interval_secs,
            def.federation.delivery_interval_secs
        );
        assert_eq!(
            parsed.features.advertise_transports,
            def.features.advertise_transports
        );
        assert_eq!(parsed.oneliners.max_length, def.oneliners.max_length);
        // The template's [seed] is all commented, so it resolves to the
        // built-in defaults.
        assert_eq!(parsed.seed.guest_password(), "guest");
        let boards = parsed.seed.boards();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].name, "General");
        // No doors are configured by default.
        assert!(parsed.doors.is_empty());
    }

    #[test]
    fn doors_parse_from_config() {
        let toml = r#"
[[doors]]
name = "Adventure"
command = "/usr/games/adventure"
time_limit_secs = 600
drop_file = "door.sys"

[[doors]]
name = "Trivia"
command = "python3"
args = ["trivia.py"]
"#;
        let s: Settings = toml::from_str(toml).unwrap();
        assert_eq!(s.doors.len(), 2);
        assert_eq!(s.doors[0].name, "Adventure");
        assert_eq!(s.doors[0].time_limit_secs, 600);
        assert_eq!(s.doors[0].drop_file.as_deref(), Some("door.sys"));
        assert_eq!(s.doors[1].args, vec!["trivia.py"]);
        // Omitted fields default.
        assert_eq!(s.doors[1].time_limit_secs, 0);
        assert!(s.doors[1].cwd.is_none());
    }

    #[test]
    fn seed_boards_and_guest_password_are_configurable() {
        let toml = r#"
[seed]
guest_password = "visitor"
boards = [
  { name = "Lobby", description = "hi", min_write = "user" },
  { name = "Staff", min_read = "admin", min_write = "admin" },
]
"#;
        let s: Settings = toml::from_str(toml).unwrap();
        assert_eq!(s.seed.guest_password(), "visitor");
        let boards = s.seed.boards();
        assert_eq!(boards.len(), 2);
        assert_eq!(boards[0].name, "Lobby");
        // Omitted fields fall back to their defaults.
        assert_eq!(boards[1].description, "");
        assert_eq!(boards[1].min_read, "admin");
        assert_eq!(boards[0].min_read, "guest");

        // An explicit empty list seeds no boards (distinct from "unset").
        let none: Settings = toml::from_str("[seed]\nboards = []\n").unwrap();
        assert!(none.seed.boards().is_empty());
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
    fn web_connect_host_prefers_hostname_then_acme_then_host() {
        let mut web = Web::default(); // host 0.0.0.0, no hostname, no acme
        assert_eq!(web.connect_host(), "localhost");

        web.host = "10.0.0.5".into();
        assert_eq!(web.connect_host(), "10.0.0.5");

        // An ACME domain is public by definition, so it beats the bind host.
        web.acme_domains = vec!["acme.example.com".into()];
        assert_eq!(web.connect_host(), "acme.example.com");

        // An explicit hostname wins over everything.
        web.hostname = "www.example.net".into();
        assert_eq!(web.connect_host(), "www.example.net");

        web.hostname = "  ".into(); // blank falls back down the chain
        assert_eq!(web.connect_host(), "acme.example.com");
    }

    #[test]
    fn federation_origin_is_validated_fail_closed() {
        // Every rejection below is permanent damage if it slipped through: an
        // AP id URI can never be rewritten once it's been delivered.
        let fed = |origin: &str| Federation {
            origin: origin.into(),
            ..Federation::default()
        };

        // The happy path, normalized (trailing slash trimmed).
        assert_eq!(
            fed("https://bbs.example.com").origin().unwrap(),
            "https://bbs.example.com"
        );
        assert_eq!(
            fed("https://bbs.example.com/").origin().unwrap(),
            "https://bbs.example.com"
        );
        // An explicit :443 is the https default, so it isn't a "port".
        assert_eq!(
            fed("https://bbs.example.com:443").origin().unwrap(),
            "https://bbs.example.com:443"
        );

        for bad in [
            "", // unset
            "   ",
            "bbs.example.com",        // no scheme
            "http://bbs.example.com", // plaintext can't interop
            "https://localhost",      // unreachable + permanent
            "https://bbs.localhost",
            "https://127.0.0.1", // IP can't back an acct: URI
            "https://[::1]",
            "https://bbs.example.com:8088", // RFC 7565: acct: has no port
            "https://example",              // not a real domain
            "https://bbs.example.com/ap",   // path
            "https://bbs.example.com/?x=1",
            "https://bbs.example.com/#f",
            "not a url",
        ] {
            assert!(
                fed(bad).origin().is_err(),
                "origin {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn federation_debug_insecure_permits_local_two_instance_testing() {
        // The escape hatch exists so two instances can federate on one machine
        // (the AP crate's local_federation example does this). It must never be
        // required for, or usable as, a real deployment.
        let dev = |origin: &str| Federation {
            origin: origin.into(),
            debug_insecure: true,
            ..Federation::default()
        };
        assert_eq!(
            dev("http://localhost:8088").origin().unwrap(),
            "http://localhost:8088"
        );
        assert!(dev("http://127.0.0.1:8089").origin().is_ok());
        // Still not a free-for-all: structural rules hold.
        assert!(dev("").origin().is_err());
        assert!(dev("https://x.example/path").origin().is_err());
    }

    #[test]
    fn web_connect_url_uses_scheme_and_omits_default_port() {
        let mut web = Web {
            hostname: "bbs.example.com".into(),
            ..Web::default()
        };
        // Non-default port is shown.
        assert_eq!(web.connect_url(), "https://bbs.example.com:8088");

        // 443 is the https default — omit it.
        web.port = 443;
        assert_eq!(web.connect_url(), "https://bbs.example.com");

        // Plain HTTP: scheme follows tls, and 80 is its default.
        web.tls = false;
        assert_eq!(web.connect_url(), "http://bbs.example.com:443");
        web.port = 80;
        assert_eq!(web.connect_url(), "http://bbs.example.com");
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
