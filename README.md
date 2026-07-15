# bbs-rs

[![CI](https://github.com/AdamIsrael/bbs-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/AdamIsrael/bbs-rs/actions/workflows/ci.yml)

A bare-bones **bulletin board system (BBS) served over SSH**, written in Rust with
[`russh`](https://crates.io/crates/russh) (SSH server), [`ratatui`](https://crates.io/crates/ratatui)
(terminal UI), and [`sqlx`](https://crates.io/crates/sqlx) + SQLite (users, boards, messages, mail).

## Features

- **Connect over SSH** — the TUI is rendered straight into the SSH channel; any `ssh` client works.
- **Accounts** — a shared limited `guest/guest` account plus in-TUI registration of real users
  (passwords hashed with argon2, stored in SQLite).
- **Public-key auth** — registered users can attach SSH keys and log in without a password.
- **Message boards** — browse boards, read messages, and (registered users) post; **reply threads**
  render as an indented conversation tree, and **unread posts** ("new since last call") are flagged
  per board and highlighted in the message list.
- **Board moderation & ACLs** — per-board read/write role requirements, lockable boards, and pin/delete
  of individual posts by admins.
- **Oneliners** — a shared "graffiti wall" of short public one-liners any registered user can append to.
- **File areas** — browsable download areas with role ACLs, per-user storage quotas, and file-type
  limits; **read text files and peek inside archives** (zip / tar.gz / gz) in the TUI, and
  **transfer over SFTP** (`sftp user@host`).
- **User profiles** — real name, location, tagline, and a **signature** shown beneath your posts;
  a profile screen also shows member-since, last-on, and post count. View others' profiles from Who's Online.
- **Private mail** — send and read user-to-user messages.
- **Who's online** — a live view of currently-connected users; open a user's profile from here.
- **Stats** — board totals, a top-posters leaderboard, and a recent-callers list.
- **Door games** — launch operator-configured external programs on a pseudo-terminal (full-screen ANSI,
  `isatty`), with the user's info in the environment + an optional classic drop file, and a time limit.
- **Full-text search** — keyword search across board messages (SQLite FTS5), scoped to boards you can read;
  jump straight from a hit to the message.
- **Guest guardrails** — the guest account is read-only: no posting, no mail.
- **Access control** — three roles (`guest` / `user` / `admin`); admins get an in-BBS admin view.
- **Bans** — ban/unban by username or IP; a ban rejects new logins *and* kicks any live session.
- **Login audit** — every attempt (success or failure) is recorded with username, IP, and time.
- **`bbsctl`** — an operator CLI for user management that works even when the server is down.
- **Browser frontend** — an optional WebSocket + xterm.js web terminal (`[web] enabled = true`) that
  reuses the whole TUI: same screens, same auth, same who's-online. xterm.js is vendored (self-contained).
- **Configurable** — a `bbs.toml` file customizes branding, network/SSH tuning, and feature toggles,
  with **hot reload**: edit the file (or send `SIGHUP`) and new sessions pick it up without a restart.
- **Themes & ANSI art** — pick a built-in color preset (or override individual colors), and drop in a
  custom ANSI/text welcome screen and per-screen art (CP437 `.ans` or UTF-8 both work).

## Run it

```sh
cargo run --bin bbs-rs        # or: just run   (the crate builds two binaries: bbs-rs + bbsctl)
# then, from another terminal:
ssh guest@localhost -p 2222   # password: guest
```

On first run the server writes a commented `bbs.toml`, creates `bbs.db` (SQLite), and generates a
persistent ed25519 host key (`host_key`). Register an account from the guest session
(main menu → *Register New Account*), then reconnect over SSH as that user for full access.

## Configuration

Settings live in **`bbs.toml`** (created with documented defaults on first run). Precedence is
**defaults < file < CLI flag**. CLI flags:

```
--config <PATH>        config file (default bbs.toml)
--host <ADDR>          override network.host
--port <PORT>          override network.port
--database-url <URL>   override network.database_url
--host-key <PATH>      override network.host_key
```

The file has these sections:

```toml
[bbs]        # branding
name = "bbs-rs"
tagline = "a tiny bulletin board over SSH"
sysop = ""                       # shown in help footer (blank hides)
welcome = "Welcome to the board."   # MOTD on the main menu (blank hides)

[network]    # host, port, database_url, host_key, plus:
hostname = ""                    # public hostname for connect hints (blank → host/localhost)
inactivity_timeout_secs = 3600
auth_rejection_time_secs = 2
ban_sweep_interval_secs = 10
default_cols = 80
default_rows = 24

[features]   # turn parts of the BBS off
registration = true    # in-TUI account creation (from the guest session)
guest = true           # allow the shared guest account to log in
private_mail = true
who_online = true
oneliners = true       # the graffiti wall
pubkey_auth = true     # allow SSH public-key login (users register keys in the BBS)
file_areas = true      # browse downloadable file areas

[abuse]      # auto-ban IPs with repeated failed logins
max_failures = 10      # failures within the window to trigger a ban (0 disables)
window_secs = 600      # sliding window for counting failures
ban_secs = 3600        # how long an auto-ban lasts (0 = permanent)

[accounts]   # registration policy
# Usernames that may not be registered (case-insensitive, whitespace-trimmed).
# "guest" is always reserved regardless of this list.
reserved_usernames = ["root", "admin"]

[limits]     # per-user rate limits + content length caps (admins bypass rates; 0 = off)
window_secs = 60       # sliding window for counting a user's recent actions
max_posts = 5          # board posts per user per window
max_mail = 10          # mail sent per user per window
max_oneliners = 8      # oneliners per user per window
max_subject_chars = 120  # max chars in a post/mail subject
max_body_chars = 8000    # max chars in a post/mail body

[files]      # file-area storage policy
storage_dir = "files"          # where uploaded file blobs live
max_file_bytes = 10485760      # per-file cap (0 = unlimited), default 10 MiB
user_quota_bytes = 104857600   # per-user total (0 = unlimited), default 100 MiB
allowed_extensions = []        # lowercase, no dot; empty allows any, e.g. ["txt","zip"]
max_preview_bytes = 262144     # cap when reading/decompressing a file preview in the BBS
max_archive_entries = 1000     # cap when listing an archive's entries

[theme]      # colors (fully operator-customizable)
preset = "classic"     # base: classic (default), mono, amber, matrix
# Override any single color (name / "#ff8800" / 256-index "208"):
# accent = "cyan"      # headings, tags, author names
# highlight = "green"  # "new"/unread markers
# title_fg / title_bg / warning_fg / warning_bg / dim

[art]        # operator ANSI/text art (UTF-8 or classic CP437 .ans)
dir = "art"            # art files live here (relative to the working dir)
welcome = ""           # file shown on the main menu (blank = none)
# [art.screens]        # optional per-screen header art (file per screen key)
# board_list = "boards.ans"
# file_areas = "files.ans"

[web]        # optional browser frontend (WebSocket + xterm.js), off by default
enabled = false
host = "0.0.0.0"
port = 8088

[oneliners]  # graffiti-wall policy (separate from the [features] on/off toggle)
max_entries = 200      # trim to the most recent N after each post (0 = keep all)
max_length = 120       # max chars per oneliner (0 = no cap)

[seed]       # first-run seeded content (boards created only on a fresh DB)
# guest_password = "guest"     # password for the shared guest account
# boards = [                   # replaces the default General + Announcements
#   { name = "General", description = "General chatter", min_write = "user" },
#   { name = "Staff", min_read = "admin", min_write = "admin" },
# ]

# [[doors]]  # external "door" programs (a Doors menu appears when any exist)
# name = "Adventure"
# command = "/usr/games/adventure"
# args = []
# cwd = "/var/bbs/doors/adventure"
# time_limit_secs = 900        # 0 = no limit
# drop_file = "dorinfo1.def"   # or "door.sys"; blank = none
```

**Themes** are fully customizable: pick a built-in `preset` and/or override individual colors.
**Art** lets you drop in a welcome screen and per-screen headers — real CP437 `.ans` files and modern
UTF-8 text with ANSI color escapes both render. See [`art/welcome.example.txt`](art/welcome.example.txt)
for a starting point (`welcome = "welcome.example.txt"` to use it).

**Browser frontend**: set `[web] enabled = true` and browse to `http://<host>:<port>/` for the same BBS
in a web terminal (try `guest` / `guest`). It shares the SSH server's users, login audit, bans, and
who's-online. xterm.js ([MIT](https://github.com/xtermjs/xterm.js)) is vendored under `src/web/static/`,
so the page is fully self-contained — no CDN at runtime.

Note: disabling `guest` while keeping `registration` on leaves no way for a newcomer to get in
(registration is reached from the guest session). `bbsctl` reads the same `bbs.toml` for its database
URL (`bbsctl --config bbs.toml …`), or takes `--database-url` directly.

**Hot reload**: edit `bbs.toml` while the server runs (or send it `SIGHUP`) and it re-reads the file —
no restart, no dropped sessions. **New** logins pick up the change; existing sessions keep the settings
they started with. Reloadable: branding, theme/art, `[features]`, `[limits]`, `[abuse]`, `[accounts]`,
`[files]`, `[oneliners]`. The listeners (`[network]`, `[web]`), host key, `database_url`, and `[seed]`
are bound once at startup — a reload applies but logs that those need a restart to take effect. A file
that fails to parse is rejected and the running config is kept.

Set `RUST_LOG=info` for server logs (written to stderr, never into a client's terminal).

## Navigation

`↑/↓` move · `Enter` select/open · `Esc`/`←`/`q` back · `Ctrl-C` disconnect. In forms, `Tab`/`↑`/`↓`
switch fields and `Enter` submits on the last field.

## Administration

Users have one of three roles: `guest`, `user`, or `admin`. Manage users with the **`bbsctl`** CLI
(operates on the same SQLite database as the server):

```sh
bbsctl users                     # list users (role + ban status)
bbsctl role <user> admin         # promote/demote (guest|user|admin)
bbsctl ban <user>                # ban / unban a user
bbsctl unban <user>
bbsctl ban-ip <ip> [--reason R]  # ban / unban an IP
bbsctl unban-ip <ip>
bbsctl ip-bans                   # list IP bans
bbsctl keys <user>               # list a user's SSH public keys
bbsctl add-key <user> "<ssh-… key line>" [--label L]   # register a key (or --file <path>)
bbsctl rm-key <id>               # remove a registered key
bbsctl logins [--user U] [--failures] [--limit N]   # login audit trail
bbsctl bulletins                 # list sysop bulletins
bbsctl post-bulletin <title> --body <text>          # post a bulletin
bbsctl rm-bulletin <id>          # remove a bulletin
bbsctl oneliners [--limit N]     # list recent oneliners (graffiti wall)
bbsctl rm-oneliner <id>          # remove a oneliner (moderation)
bbsctl boards                    # list boards with read/write ACLs and lock state
bbsctl set-board <name> [--read ROLE] [--write ROLE] [--lock|--unlock]   # configure a board
bbsctl file-areas                # list file areas with ACLs
bbsctl add-area <name> [--desc D] [--read ROLE] [--write ROLE]   # create a file area
bbsctl rm-area <name>            # remove an empty file area
bbsctl files <area>              # list files in an area
bbsctl add-file <area> <user> <path> [--desc D]   # add a file (copied into storage_dir)
bbsctl rm-file <id>              # remove a file (and its stored blob)
bbsctl set-file-desc <id> <text> # set a file's description (SFTP uploads have none)
bbsctl backup [--out DIR] [--files]   # snapshot the DB (and optionally file blobs)
```

**Backups** (`bbsctl backup`) use SQLite's online `VACUUM INTO`, so they run **while the server is up**
— no downtime. Each run writes a timestamped `DIR/bbs-<stamp>.db` (default `DIR` = `backups/`); with
`--files` it also copies the file-area `storage_dir` to `DIR/files-<stamp>/`. `backup` never applies
migrations or writes to the live database. Schedule it with cron/systemd for regular snapshots.

Point it at a non-default database with `--database-url`. To create your **first admin**, register a
normal account, then run `bbsctl role <that-user> admin`. Registration refuses reserved usernames —
`root` and `admin` by default (plus `guest` always), configurable via `[accounts].reserved_usernames`.

A ban rejects future logins *and* drops any live session for that user/IP (immediately for in-BBS
admin bans; within ~10s for `bbsctl` bans, via the server's ban sweeper). `admin`-role users also get
an in-BBS **Admin** menu to list users, ban/unban, and view recent logins. Every login attempt
(success or failure) is recorded with username, IP, and timestamp.

**Public-key auth.** Registered users can log in with an SSH key instead of a password. Register a key
from the in-BBS **SSH Keys** menu (press `n`, then paste your `~/.ssh/id_ed25519.pub` line), or an
operator can add one with `bbsctl add-key <user> "<key line>"`. Thereafter `ssh <user>@host -p 2222`
authenticates with that key — russh verifies the signature, and the BBS accepts it iff the key's SHA256
fingerprint is registered to that account (and the account/IP isn't banned). Guests can't own keys, and
the whole mechanism can be turned off with `[features].pubkey_auth = false`.

**Rate limiting.** Regular users are throttled per `[limits]`: at most `max_posts` board posts,
`max_mail` mails, and `max_oneliners` oneliners within a rolling `window_secs` (counted from their own
recent rows — no extra table). Over the cap, the action is refused with a "slow down" message until the
window clears. Admins are never throttled, and any cap set to `0` disables that limit. This pairs with
the auto-ban guard below to blunt scripted spam.

**Auto-ban.** The ban sweeper also watches the login audit trail and temporarily bans any IP that
exceeds `[abuse].max_failures` failed logins within `window_secs` (a fail2ban-style guard against
brute-force / bot traffic). Auto-bans expire after `ban_secs` and are purged automatically; manual
`bbsctl ban-ip` bans stay permanent. Set `max_failures = 0` to disable.

**Bulletins** are short sysop announcements posted with `bbsctl post-bulletin`. When any exist, a
session lands on the **Bulletins** screen right after login (in addition to the `bbs.welcome` MOTD);
they're also reachable any time from the main menu.

**Oneliners** are a shared "graffiti wall" of short public one-liners (up to 120 chars). Any registered
user can append one from the **Oneliners** menu (press `n`); guests are read-only, like on the boards.
Sysops can prune the wall with `bbsctl rm-oneliner <id>`, and the whole feature can be turned off with
`[features].oneliners = false`.

**File areas.** Downloadable files are grouped into **areas**, each with a read/write role ACL like a
board. Registered users browse areas and files from the **File Areas** menu and view per-file details
(size, uploader, description, download count). This first phase is a **catalog with operator-managed
storage**: sysops create areas with `bbsctl add-area` and add files from a server path with
`bbsctl add-file <area> <user> <path>`, which copies the blob into `[files].storage_dir` and records it.
Uploads are checked against the **allowed extensions**, the **per-file size cap**, and the uploader's
**storage quota** (`[files]`); admins are exempt from the quota (an operator seeding an area is
effectively an admin).

From a file's detail screen, press `Enter` to **view it in the BBS**: text files open in a scrollable
pager, and archives (`.zip`, `.tar.gz`/`.tgz`, `.gz`) show their entries — pick a text entry to read it
inline. Binary files (and binary entries) say so rather than dumping garbage. Previews are bounded by
`[files].max_preview_bytes` / `max_archive_entries` and stream from the stored blob (nothing is
extracted to disk).

Users **transfer files over SFTP** — the server answers the `sftp` subsystem with a small virtual
filesystem: `/` lists the areas you can read as directories, `/<area>/` lists its files, and
`/<area>/<file>` is a file. So:

```sh
sftp -P 2222 you@localhost
sftp> ls                       # your readable areas
sftp> cd Uploads
sftp> get somefile.zip         # download (honors the area's read role)
sftp> put local.txt            # upload (honors write role + extension/size/quota)
```

Reads honor each area's `min_read_role`; uploads honor `min_write_role` plus the `[files]` limits and
count against your quota. SFTP `put` can't carry a **description**, so uploads start with none — the
uploader (or an admin) adds one from the file's detail screen in the BBS (press `e`), or an operator
runs `bbsctl set-file-desc <id> <text>`. SFTP auth is the same as the BBS (password or a registered public key), and
the whole feature follows `[features].file_areas`.

**Board moderation & ACLs.** Each board has a minimum **read** and **write** role (`guest` < `user` <
`admin`) and a **locked** flag. Defaults preserve the classic behavior — anyone may read, registered
users may post — and the seeded *Announcements* board is admin-only to post to. Configure boards with
`bbsctl set-board <name> --read <role> --write <role>` (or `--lock`/`--unlock`); `bbsctl boards` shows
the current settings. In the BBS, `admin`-role users get extra keys on the board screens: `l` to
lock/unlock the selected board, and on a board's message list `p` to pin/unpin and `d` to delete the
selected post. Pinned posts sort to the top. A locked board rejects new posts from regular users
(admins can still post, e.g. to add a closing note); boards a user can't read are hidden from their
board list.

## Upgrading & migrations

Migrations are compiled into the binary and run automatically when the server starts. To apply them
explicitly — e.g. from a released binary during a maintenance window, without the source tree — use
either binary:

```sh
bbs-rs --migrate         # apply pending migrations, then exit (does NOT start the server)
bbsctl migrate           # apply pending migrations
bbsctl migrate --status  # list applied/pending migrations without applying anything
```

## Architecture

The TUI is deliberately **transport-agnostic** so a future HTTP(S) frontend can reuse it:

```
ssh::server (russh Handler)  ─┐
                              ├─ Transport contract ─▶ app::run (ratatui state machine)
web (WebSocket + xterm.js) ──┘        (byte sink + Event stream)   │
   [future]                                                        ▼
                                                    services / db (SQLite)
```

- `services` + `db` hold all domain logic and know nothing about SSH.
- `app` is the ratatui state machine and event loop, generic over any `Write` byte sink.
- `transport` + `input` define the byte-sink / decoded-`Event` contract.
- `ssh` adapts russh to that contract (a `TerminalHandle` that ships bytes to the channel, and a
  stateful input parser that reassembles escape sequences split across packets).

Because ratatui/crossterm emit the same ANSI that xterm.js understands, a WebSocket frontend can
implement the same contract and reuse the entire application unchanged.

## Tests

```sh
cargo test        # input-parser unit tests + service integration tests (in-memory SQLite)
```

## Developer tasks

Common tasks are wrapped in a [`justfile`](justfile) (run [`just`](https://github.com/casey/just)
with no args to list them):

```sh
just run          # run the server
just test         # run the test suite
just lint         # clippy (warnings as errors) + rustfmt check
just fmt          # format the source
just reset-db     # delete bbs.db (recreated + seeded on next run)
just ci           # fmt + lint + test
```

## CI & releases

[GitHub Actions](.github/workflows) run `fmt`/`clippy`/`build`/`test` on every push and PR. Pushing a
version tag builds release binaries (`bbs-rs` + `bbsctl`) for Linux and macOS and publishes them as a
GitHub Release:

```sh
git tag v0.1.0 && git push origin v0.1.0
```
