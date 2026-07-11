# sshtui

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

## Run it

```sh
cargo run --bin sshtui        # or: just run   (the crate builds two binaries: sshtui + bbsctl)
# then, from another terminal:
ssh guest@localhost -p 2222   # password: guest
```

On first run the server creates `bbs.db` (SQLite) and generates a persistent ed25519 host key
(`host_key`). Register an account from the guest session (main menu → *Register New Account*), then
reconnect over SSH as that user for full access.

### Options

```
--host <ADDR>          bind address (default 0.0.0.0)
--port <PORT>          SSH port (default 2222)
--database-url <URL>   SQLite URL (default sqlite://bbs.db?mode=rwc)
--host-key <PATH>      host key path (default host_key)
```

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
```

Point it at a non-default database with `--database-url`. To create your **first admin**, register a
normal account, then run `bbsctl role <that-user> admin`.

A ban rejects future logins *and* drops any live session for that user/IP (immediately for in-BBS
admin bans; within ~10s for `bbsctl` bans, via the server's ban sweeper). `admin`-role users also get
an in-BBS **Admin** menu to list users, ban/unban, and view recent logins. Every login attempt
(success or failure) is recorded with username, IP, and timestamp.

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
