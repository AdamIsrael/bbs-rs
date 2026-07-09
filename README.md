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

## Run it

```sh
cargo run
# then, from another terminal:
ssh guest@localhost -p 2222      # password: guest
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
