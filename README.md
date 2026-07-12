# bbs-rs

[![CI](https://github.com/AdamIsrael/bbs-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/AdamIsrael/bbs-rs/actions/workflows/ci.yml)

A bare-bones **bulletin board system (BBS) served over SSH**, written in Rust with
[`russh`](https://crates.io/crates/russh) (SSH server), [`ratatui`](https://crates.io/crates/ratatui)
(terminal UI), and [`sqlx`](https://crates.io/crates/sqlx) + SQLite (users, boards, messages, mail).

## Features

- **Connect over SSH** — the TUI is rendered straight into the SSH channel; any `ssh` client works.
- **Accounts** — a shared limited `guest/guest` account plus in-TUI registration of real users
  (passwords hashed with argon2, stored in SQLite).
- **Message boards** — browse boards, read messages, and (registered users) post.
- **Private mail** — send and read user-to-user messages.
- **Who's online** — a live view of currently-connected users.
- **Guest guardrails** — the guest account is read-only: no posting, no mail.
- **Access control** — three roles (`guest` / `user` / `admin`); admins get an in-BBS admin view.
- **Bans** — ban/unban by username or IP; a ban rejects new logins *and* kicks any live session.
- **Login audit** — every attempt (success or failure) is recorded with username, IP, and time.
- **`bbsctl`** — an operator CLI for user management that works even when the server is down.
- **Configurable** — a `bbs.toml` file customizes branding, network/SSH tuning, and feature toggles.

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

The file has three sections:

```toml
[bbs]        # branding
name = "bbs-rs"
tagline = "a tiny bulletin board over SSH"
sysop = ""                       # shown in help footer (blank hides)
welcome = "Welcome to the board."   # MOTD on the main menu (blank hides)

[network]    # host, port, database_url, host_key, plus:
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

[abuse]      # auto-ban IPs with repeated failed logins
max_failures = 10      # failures within the window to trigger a ban (0 disables)
window_secs = 600      # sliding window for counting failures
ban_secs = 3600        # how long an auto-ban lasts (0 = permanent)
```

Note: disabling `guest` while keeping `registration` on leaves no way for a newcomer to get in
(registration is reached from the guest session). `bbsctl` reads the same `bbs.toml` for its database
URL (`bbsctl --config bbs.toml …`), or takes `--database-url` directly.

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
bbsctl logins [--user U] [--failures] [--limit N]   # login audit trail
bbsctl bulletins                 # list sysop bulletins
bbsctl post-bulletin <title> --body <text>          # post a bulletin
bbsctl rm-bulletin <id>          # remove a bulletin
```

Point it at a non-default database with `--database-url`. To create your **first admin**, register a
normal account, then run `bbsctl role <that-user> admin`.

A ban rejects future logins *and* drops any live session for that user/IP (immediately for in-BBS
admin bans; within ~10s for `bbsctl` bans, via the server's ban sweeper). `admin`-role users also get
an in-BBS **Admin** menu to list users, ban/unban, and view recent logins. Every login attempt
(success or failure) is recorded with username, IP, and timestamp.

**Auto-ban.** The ban sweeper also watches the login audit trail and temporarily bans any IP that
exceeds `[abuse].max_failures` failed logins within `window_secs` (a fail2ban-style guard against
brute-force / bot traffic). Auto-bans expire after `ban_secs` and are purged automatically; manual
`bbsctl ban-ip` bans stay permanent. Set `max_failures = 0` to disable.

**Bulletins** are short sysop announcements posted with `bbsctl post-bulletin`. When any exist, a
session lands on the **Bulletins** screen right after login (in addition to the `bbs.welcome` MOTD);
they're also reachable any time from the main menu.

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
