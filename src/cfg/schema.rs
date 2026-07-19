//! What `bbs.toml` contains, described so a UI can render it (#141).
//!
//! One entry per editable setting: its type, a one-line explanation, and — for
//! numbers — the range that makes sense. The help text is condensed from the
//! comments in the shipped config, because a config UI is exactly where that
//! reasoning belongs.
//!
//! **Not everything in the file is here, deliberately.** `[[doors]]`,
//! `[art.screens]`, and `[seed] boards` are arrays of tables and nested tables;
//! editing those well needs a different UI than a field list, and half-editing
//! them would be worse than leaving them alone. Because [`super::doc`] edits in
//! place, anything absent from this schema is simply preserved untouched.

/// What kind of value a setting holds, and how to edit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Bool,
    /// Inclusive bounds. `0` is frequently a sentinel ("unlimited"), so the
    /// minimum is usually 0 rather than 1 — the help text says what it means.
    Int {
        min: i64,
        max: i64,
    },
    Str,
    /// A filesystem path. Same editing as `Str`; separate so a UI can offer
    /// completion or flag an unwritable directory.
    Path,
    /// One of a fixed set.
    Enum(&'static [&'static str]),
    /// A list of strings (TOML array).
    StrList,
}

/// One editable setting.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub help: &'static str,
}

/// How a section is edited.
///
/// Most sections are a flat list of settings. `[[doors]]` is an **array of
/// tables** — a list of entries, each with its own fields — which needs list
/// operations (add, remove, reorder) that a field list has no place for (#145).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// A flat list of settings.
    Fields,
    /// A list of entries, each edited with [`DOOR_FIELDS`].
    Doors,
    /// The `[art.screens]` table: a fixed set of known keys, each holding a
    /// filename (#146).
    ArtScreens,
}

/// A `[section]` of the config.
pub struct Section {
    pub name: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    /// Whether changing anything here needs a restart.
    ///
    /// Mirrors `reload::warn_restart_only`: the listeners, host key, database,
    /// federation config and one-time seeding are bound at startup, so a reload
    /// applies them to the shared settings but they don't take effect. Everything
    /// else reaches new sessions on the next connect.
    pub restart_only: bool,
    pub kind: SectionKind,
    pub fields: &'static [Field],
}

const ROLES: &[&str] = &["guest", "user", "admin"];
/// Blank means "write no drop file", which is why it's an option rather than an
/// absent value — the operator picks it explicitly.
const DROP_FILES: &[&str] = &["", "door.sys", "dorinfo1.def"];

/// The fields of one `[[doors]]` entry (#145).
pub static DOOR_FIELDS: &[Field] = &[
    Field {
        key: "name",
        label: "Menu label",
        kind: FieldKind::Str,
        help: "What callers see in the Doors menu.",
    },
    Field {
        key: "command",
        label: "Command",
        kind: FieldKind::Path,
        help: "The program to run. It gets a pseudo-terminal and the caller's details in the environment (BBS_USER, BBS_TIME_LEFT_SECS, ...).",
    },
    Field {
        key: "args",
        label: "Arguments",
        kind: FieldKind::StrList,
        help: "Passed to the program, comma-separated here. Each is one argument — they are not run through a shell, so quoting and globs do not apply.",
    },
    Field {
        key: "cwd",
        label: "Working directory",
        kind: FieldKind::Path,
        help: "Where the program runs, and where a drop file is written. Blank uses the BBS's own working directory.",
    },
    Field {
        key: "time_limit_secs",
        label: "Time limit (s)",
        kind: FieldKind::Int {
            min: 0,
            max: 86_400,
        },
        help: "Kill the program after this long. 0 means no limit.",
    },
    Field {
        key: "drop_file",
        label: "Drop file",
        kind: FieldKind::Enum(DROP_FILES),
        help: "Classic BBS handoff file written before launch, for doors that expect one. Blank writes none.",
    },
];
const PRESETS: &[&str] = &["classic", "mono", "amber", "matrix"];

pub static SECTIONS: &[Section] = &[
    Section {
        name: "bbs",
        title: "Board identity",
        help: "Names and text shown to callers.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "name",
                label: "Board name",
                kind: FieldKind::Str,
                help: "Shown in the title bar, startup log, and help.",
            },
            Field {
                key: "tagline",
                label: "Tagline",
                kind: FieldKind::Str,
                help: "Short subtitle on the main menu and help screen.",
            },
            Field {
                key: "sysop",
                label: "Sysop name",
                kind: FieldKind::Str,
                help: "Shown in the help footer. Blank hides it.",
            },
            Field {
                key: "welcome",
                label: "Welcome message",
                kind: FieldKind::Str,
                help: "Message-of-the-day banner on the main menu. Blank hides it.",
            },
        ],
    },
    Section {
        name: "network",
        title: "SSH & database",
        help: "Where the BBS listens and what it stores to. CLI flags override these.",
        restart_only: true,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "host",
                label: "Bind address",
                kind: FieldKind::Str,
                help: "0.0.0.0 listens on every interface; 127.0.0.1 is local-only.",
            },
            Field {
                key: "port",
                label: "SSH port",
                kind: FieldKind::Int { min: 1, max: 65535 },
                help: "Ports below 1024 need privilege to bind.",
            },
            Field {
                key: "hostname",
                label: "Public hostname",
                kind: FieldKind::Str,
                help: "What callers type to reach you, shown in connect hints. Blank falls back to the bind address.",
            },
            Field {
                key: "database_url",
                label: "Database URL",
                kind: FieldKind::Str,
                help: "SQLite URL, e.g. sqlite://bbs.db?mode=rwc. Changing this points the board at a different database.",
            },
            Field {
                key: "host_key",
                label: "SSH host key path",
                kind: FieldKind::Path,
                help: "Generated on first run if missing. Replacing it makes every client warn about a changed key.",
            },
            Field {
                key: "inactivity_timeout_secs",
                label: "Idle timeout (s)",
                kind: FieldKind::Int {
                    min: 0,
                    max: 86_400,
                },
                help: "Disconnect idle sessions after this long. 0 disables.",
            },
            Field {
                key: "auth_rejection_time_secs",
                label: "Auth reject delay (s)",
                kind: FieldKind::Int { min: 0, max: 60 },
                help: "Pause before returning an SSH auth failure; slows brute force.",
            },
            Field {
                key: "ban_sweep_interval_secs",
                label: "Ban sweep interval (s)",
                kind: FieldKind::Int { min: 1, max: 3_600 },
                help: "How often to look for banned users/IPs and kick their live sessions.",
            },
            Field {
                key: "default_cols",
                label: "Fallback columns",
                kind: FieldKind::Int { min: 20, max: 500 },
                help: "Terminal width used when a client reports 0x0.",
            },
            Field {
                key: "default_rows",
                label: "Fallback rows",
                kind: FieldKind::Int { min: 10, max: 200 },
                help: "Terminal height used when a client reports 0x0.",
            },
        ],
    },
    Section {
        name: "features",
        title: "Features",
        help: "Turn whole areas of the BBS on or off.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "registration",
                label: "Allow registration",
                kind: FieldKind::Bool,
                help: "Let callers create accounts from the guest session.",
            },
            Field {
                key: "guest",
                label: "Allow guest login",
                kind: FieldKind::Bool,
                help: "The shared read-only guest account. Guests never federate.",
            },
            Field {
                key: "private_mail",
                label: "Private mail",
                kind: FieldKind::Bool,
                help: "User-to-user mail.",
            },
            Field {
                key: "who_online",
                label: "Who's online",
                kind: FieldKind::Bool,
                help: "The who's-online view.",
            },
            Field {
                key: "oneliners",
                label: "Oneliners",
                kind: FieldKind::Bool,
                help: "The graffiti wall. Also this board's ActivityPub statuses when federation is on.",
            },
            Field {
                key: "pubkey_auth",
                label: "SSH public-key auth",
                kind: FieldKind::Bool,
                help: "Let users register SSH keys and log in with them.",
            },
            Field {
                key: "file_areas",
                label: "File areas",
                kind: FieldKind::Bool,
                help: "Browsable, downloadable file areas.",
            },
            Field {
                key: "advertise_transports",
                label: "Advertise the other way in",
                kind: FieldKind::Bool,
                help: "SSH users see the web URL and vice versa. Set the hostname fields so the address is reachable.",
            },
        ],
    },
    Section {
        name: "abuse",
        title: "Abuse protection",
        help: "Automatic IP bans after repeated failed logins.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "max_failures",
                label: "Failures before ban",
                kind: FieldKind::Int { min: 0, max: 1_000 },
                help: "Auto-ban an IP after this many failed logins in the window. 0 disables auto-banning.",
            },
            Field {
                key: "window_secs",
                label: "Failure window (s)",
                kind: FieldKind::Int {
                    min: 1,
                    max: 86_400,
                },
                help: "Sliding window over which failures are counted.",
            },
            Field {
                key: "ban_secs",
                label: "Ban duration (s)",
                kind: FieldKind::Int {
                    min: 0,
                    max: 31_536_000,
                },
                help: "How long an auto-ban lasts. 0 is permanent.",
            },
        ],
    },
    Section {
        name: "accounts",
        title: "Accounts",
        help: "Registration policy.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[Field {
            key: "reserved_usernames",
            label: "Reserved usernames",
            kind: FieldKind::StrList,
            help: "Names nobody may register (case-insensitive). \"guest\" is always reserved. Usernames containing @ are always refused, to stop impersonation of remote actors.",
        }],
    },
    Section {
        name: "limits",
        title: "Rate limits",
        help: "Per-user caps. Admins are never throttled; 0 disables a cap.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "window_secs",
                label: "Window (s)",
                kind: FieldKind::Int {
                    min: 1,
                    max: 86_400,
                },
                help: "The period the caps below are counted over.",
            },
            Field {
                key: "max_posts",
                label: "Posts per window",
                kind: FieldKind::Int {
                    min: 0,
                    max: 10_000,
                },
                help: "Board posts. Also applies to remote authors posting into your boards.",
            },
            Field {
                key: "max_mail",
                label: "Mail per window",
                kind: FieldKind::Int {
                    min: 0,
                    max: 10_000,
                },
                help: "Messages sent.",
            },
            Field {
                key: "max_oneliners",
                label: "Oneliners per window",
                kind: FieldKind::Int {
                    min: 0,
                    max: 10_000,
                },
                help: "Matters more now the wall no longer auto-trims.",
            },
            Field {
                key: "max_subject_chars",
                label: "Max subject length",
                kind: FieldKind::Int {
                    min: 0,
                    max: 10_000,
                },
                help: "Characters in a post or mail subject. 0 disables.",
            },
            Field {
                key: "max_body_chars",
                label: "Max body length",
                kind: FieldKind::Int {
                    min: 0,
                    max: 1_000_000,
                },
                help: "Characters in a post or mail body. 0 disables.",
            },
        ],
    },
    Section {
        name: "files",
        title: "File areas",
        help: "Storage and limits for uploaded files.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "storage_dir",
                label: "Storage directory",
                kind: FieldKind::Path,
                help: "Where file blobs are kept, relative to the working directory.",
            },
            Field {
                key: "max_file_bytes",
                label: "Max file size (bytes)",
                kind: FieldKind::Int {
                    min: 0,
                    max: 107_374_182_400,
                },
                help: "Largest single file. 0 is unlimited. Default is 10 MiB.",
            },
            Field {
                key: "user_quota_bytes",
                label: "Per-user quota (bytes)",
                kind: FieldKind::Int {
                    min: 0,
                    max: 1_099_511_627_776,
                },
                help: "Total one user may store. 0 is unlimited. Default is 100 MiB.",
            },
            Field {
                key: "allowed_extensions",
                label: "Allowed extensions",
                kind: FieldKind::StrList,
                help: "Lowercase, no dot, e.g. txt zip png. Empty allows anything.",
            },
            Field {
                key: "max_preview_bytes",
                label: "Max preview (bytes)",
                kind: FieldKind::Int {
                    min: 0,
                    max: 104_857_600,
                },
                help: "How much is read or decompressed when previewing a file or archive entry.",
            },
            Field {
                key: "max_archive_entries",
                label: "Max archive entries",
                kind: FieldKind::Int {
                    min: 0,
                    max: 100_000,
                },
                help: "How many entries to list from an archive.",
            },
        ],
    },
    Section {
        name: "theme",
        title: "Theme",
        help: "Colors. Pick a preset, then override individual colors if you want. A color is a name (cyan, darkgray, …), a 256-palette index (\"208\"), or hex (\"#ff8800\").",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "preset",
                label: "Preset",
                kind: FieldKind::Enum(PRESETS),
                help: "The built-in base palette.",
            },
            Field {
                key: "title_fg",
                label: "Title text",
                kind: FieldKind::Str,
                help: "Title-bar text color.",
            },
            Field {
                key: "title_bg",
                label: "Title background",
                kind: FieldKind::Str,
                help: "Title-bar background color.",
            },
            Field {
                key: "accent",
                label: "Accent",
                kind: FieldKind::Str,
                help: "Headings, tags, author names.",
            },
            Field {
                key: "highlight",
                label: "Highlight",
                kind: FieldKind::Str,
                help: "New/unread markers.",
            },
            Field {
                key: "warning_fg",
                label: "Warning text",
                kind: FieldKind::Str,
                help: "Status and warning text.",
            },
            Field {
                key: "warning_bg",
                label: "Warning background",
                kind: FieldKind::Str,
                help: "Status and warning background.",
            },
            Field {
                key: "dim",
                label: "Dim",
                kind: FieldKind::Str,
                help: "Secondary text, hints, labels.",
            },
        ],
    },
    Section {
        name: "art",
        title: "ANSI art",
        help: "CP437 .ans art and UTF-8 text with ANSI escapes both work. Per-screen art ([art.screens]) is edited in the file.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "dir",
                label: "Art directory",
                kind: FieldKind::Path,
                help: "Where art files live, relative to the working directory.",
            },
            Field {
                key: "welcome",
                label: "Main-menu art",
                kind: FieldKind::Str,
                help: "A file name under the art directory. Blank means none.",
            },
        ],
    },
    Section {
        name: "art.screens",
        title: "Per-screen art",
        help: "Optional art heading individual screens. Files live under the art directory set in ANSI art. A name that isn't there is flagged when you save — a missing file fails silently at runtime, the screen just renders plain.",
        restart_only: false,
        kind: SectionKind::ArtScreens,
        // The rows come from app::ART_SCREEN_KEYS, the same list the server
        // matches against, so this screen can't offer a key that gets ignored.
        fields: &[],
    },
    Section {
        name: "web",
        title: "Web frontend",
        help: "A browser terminal over WebSocket that reuses the whole TUI. Required for federation — every ActivityPub endpoint is served here.",
        restart_only: true,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "enabled",
                label: "Enable web frontend",
                kind: FieldKind::Bool,
                help: "Serve the BBS in a browser alongside SSH.",
            },
            Field {
                key: "host",
                label: "Bind address",
                kind: FieldKind::Str,
                help: "Use 127.0.0.1 when a reverse proxy sits in front.",
            },
            Field {
                key: "port",
                label: "Port",
                kind: FieldKind::Int { min: 1, max: 65535 },
                help: "Federation needs the frontend reachable on 443, because acct: URIs have no port component.",
            },
            Field {
                key: "hostname",
                label: "Public hostname",
                kind: FieldKind::Str,
                help: "What browsers type to reach you. Blank falls back to the first ACME domain, then the bind address.",
            },
            Field {
                key: "tls",
                label: "TLS",
                kind: FieldKind::Bool,
                help: "On by default; a self-signed cert is generated if none is configured. Turn off when a proxy terminates TLS.",
            },
            Field {
                key: "tls_cert",
                label: "TLS certificate",
                kind: FieldKind::Path,
                help: "Your own certificate. Leave blank to use the generated self-signed one.",
            },
            Field {
                key: "tls_key",
                label: "TLS key",
                kind: FieldKind::Path,
                help: "Private key matching the certificate above.",
            },
            Field {
                key: "acme_domains",
                label: "ACME domains",
                kind: FieldKind::StrList,
                help: "Fetch a trusted Let's Encrypt certificate for these names. Takes precedence over the cert/key above. Needs public DNS and port 443.",
            },
            Field {
                key: "acme_email",
                label: "ACME contact email",
                kind: FieldKind::Str,
                help: "Where Let's Encrypt sends expiry warnings.",
            },
            Field {
                key: "acme_cache",
                label: "ACME cache directory",
                kind: FieldKind::Path,
                help: "Persists the account key and issued certificates. Losing it means re-issuing.",
            },
            Field {
                key: "acme_staging",
                label: "Use ACME staging",
                kind: FieldKind::Bool,
                help: "Issue untrusted test certificates. Use this first — staging has far higher rate limits.",
            },
        ],
    },
    Section {
        name: "federation",
        title: "Federation (ActivityPub)",
        help: "Syndicate boards to other bbs-rs instances and make users user@host, followable from Mastodon. Requires the web frontend.",
        restart_only: true,
        kind: SectionKind::Fields,
        fields: &[
            Field {
                key: "enabled",
                label: "Enable federation",
                kind: FieldKind::Bool,
                help: "Off by default. See docs/FEDERATION-SETUP.md before turning this on.",
            },
            Field {
                key: "origin",
                label: "Public origin",
                kind: FieldKind::Str,
                help: "PERMANENT. Scheme and host only, e.g. https://bbs.example.com — no port, no path. Actor URIs are built from it and can never be rewritten once delivered; changing it later orphans every remote follow.",
            },
            Field {
                key: "allowlist_only",
                label: "Allowlist only",
                kind: FieldKind::Bool,
                help: "Federate only with domains you name via bbsctl ap-allow. On by default: open federation means volunteering to moderate the entire internet.",
            },
            Field {
                key: "allow_remote_dms",
                label: "Allow remote DMs",
                kind: FieldKind::Bool,
                help: "Off by default. Fediverse DMs are NOT private — they sit in plaintext on every server they touch. The UI labels them accordingly.",
            },
            Field {
                key: "delivery_interval_secs",
                label: "Delivery interval (s)",
                kind: FieldKind::Int { min: 1, max: 3_600 },
                help: "How often the outbound queue drains.",
            },
            Field {
                key: "delivery_max_attempts",
                label: "Delivery attempts",
                kind: FieldKind::Int { min: 1, max: 100 },
                help: "Give up on an activity after this many failures.",
            },
            Field {
                key: "debug_insecure",
                label: "Insecure mode (testing only)",
                kind: FieldKind::Bool,
                help: "Allows http, localhost and ports in the origin. LOCAL TESTING ONLY — never on a real board.",
            },
        ],
    },
    Section {
        name: "oneliners",
        title: "Oneliners",
        help: "Graffiti-wall policy, separate from the on/off toggle in Features. The wall does not auto-trim: oneliners are federated posts with permanent URIs.",
        restart_only: false,
        kind: SectionKind::Fields,
        fields: &[Field {
            key: "max_length",
            label: "Max length",
            kind: FieldKind::Int {
                min: 0,
                max: 10_000,
            },
            help: "Characters per oneliner. 0 removes the cap; 500 matches Mastodon.",
        }],
    },
    Section {
        name: "doors",
        title: "Door games",
        help: "External programs callers can run. Each gets a pseudo-terminal and the caller's details in the environment. A Doors menu appears when at least one is configured.",
        restart_only: false,
        kind: SectionKind::Doors,
        // Per-entry fields live in DOOR_FIELDS; this list is empty because a
        // door section has entries, not settings of its own.
        fields: &[],
    },
    Section {
        name: "seed",
        title: "First-run seeding",
        help: "Applied only to a fresh database. Custom boards ([seed] boards) are edited in the file.",
        restart_only: true,
        kind: SectionKind::Fields,
        fields: &[Field {
            key: "guest_password",
            label: "Guest password",
            kind: FieldKind::Str,
            help: "Password for the shared guest account.",
        }],
    },
];

impl Section {
    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }
}

/// Look up a section by its `[name]`.
pub fn section(name: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.name == name)
}

/// Roles a board ACL accepts, for the seed-board editor when it lands.
pub fn roles() -> &'static [&'static str] {
    ROLES
}
