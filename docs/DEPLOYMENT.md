# Running bbs-rs as a systemd service

The hands-on guide to running a bbs-rs board as a long-lived service on a Linux
host: a complete unit file, the sandboxing that goes with it, and the handful of
bbs-rs behaviours that will bite you if the unit doesn't account for them.

Everything in the README assumes you're running the binary in a shell. This is
what to do instead once the board is real.

> **Read this first: the working directory is load-bearing.** Several of
> bbs-rs's defaults are *relative* paths — including the SSH **host key**. Start
> the service from the wrong directory and you get a second, empty database and
> a **different host key**, which means every returning user is greeted by their
> SSH client's man-in-the-middle warning. Always set `WorkingDirectory=`. See
> [§2](#2-the-working-directory-problem).

---

## 1. Layout

A conventional setup, used by every example below:

| Path | What |
|---|---|
| `/usr/local/bin/bbs-rs` | the server binary |
| `/usr/local/bin/bbsctl` | the operator CLI |
| `/var/lib/bbs-rs/` | **state**: `bbs.toml`, `bbs.db`, `host_key`, `files/`, `acme-cache/` |
| `bbs` (system user) | an unprivileged account that owns the state directory |

Create the user and the state directory:

```sh
sudo useradd --system --home-dir /var/lib/bbs-rs --shell /usr/sbin/nologin bbs
sudo install -d -o bbs -g bbs -m 0750 /var/lib/bbs-rs
```

`0750` matters — see [§6](#6-secrets-at-rest).

Generate the initial config and database as that user, so nothing in the state
directory ends up owned by root:

```sh
sudo -u bbs bash -c 'cd /var/lib/bbs-rs && /usr/local/bin/bbs-rs --migrate'
```

That writes an annotated `bbs.toml`, creates `bbs.db`, applies all migrations,
and **exits without serving**. Edit `bbs.toml` (or run `bbsctl`'s config editor)
before starting the service.

---

## 2. The working-directory problem

These defaults are **relative to the process's working directory**:

| Setting | Default | Consequence of getting it wrong |
|---|---|---|
| `[network] database_url` | `sqlite://bbs.db?mode=rwc` | A second, empty board. Your users and posts appear to have vanished. |
| `[network] host_key` | `host_key` | **A different SSH host key** — every client shows a MITM warning and refuses to connect until the user clears it. |
| `[files] storage_dir` | `files` | Uploaded files aren't where the catalog says they are. |
| `[web] acme_cache` | `acme-cache` | ACME re-registers and re-issues, burning Let's Encrypt rate limits. |

None of these fail loudly. SQLite happily creates a new database, and bbs-rs
happily generates a new host key. You get a board that *starts fine* and is
quietly wrong.

Two defences, and you want both:

1. **`WorkingDirectory=/var/lib/bbs-rs`** in the unit.
2. **Absolute paths in `bbs.toml`**, so a mistake in the unit can't matter:

```toml
[network]
database_url = "sqlite:///var/lib/bbs-rs/bbs.db?mode=rwc"
host_key     = "/var/lib/bbs-rs/host_key"

[files]
storage_dir = "/var/lib/bbs-rs/files"

[web]
acme_cache = "/var/lib/bbs-rs/acme-cache"
```

**Back up `host_key` along with the database.** Losing it is not a data-loss
event, but it *is* a "every user gets a scary warning" event.

---

## 3. The unit file

`/etc/systemd/system/bbs-rs.service`:

```ini
[Unit]
Description=bbs-rs bulletin board
Documentation=https://github.com/AdamIsrael/bbs-rs
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bbs
Group=bbs
WorkingDirectory=/var/lib/bbs-rs
ExecStart=/usr/local/bin/bbs-rs --config /var/lib/bbs-rs/bbs.toml

# Re-read bbs.toml without dropping sessions. See §4 for what this does and
# does not pick up.
ExecReload=/bin/kill -HUP $MAINPID

Restart=on-failure
RestartSec=5s

# Logs go to stderr, and from there to the journal. Never into a client's
# terminal.
Environment=RUST_LOG=info

# --- Ports below 1024 -------------------------------------------------------
# Only needed if you actually bind one; see §5. Grant nothing you don't use.
#AmbientCapabilities=CAP_NET_BIND_SERVICE
#CapabilityBoundingSet=CAP_NET_BIND_SERVICE

# --- Sandboxing (see §7 before changing) ------------------------------------
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/bbs-rs
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
ProtectHostname=true
ProtectProc=invisible
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
UMask=0027

[Install]
WantedBy=multi-user.target
```

Enable and start it:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now bbs-rs
systemctl status bbs-rs
```

---

## 4. Reloading configuration

`SIGHUP` re-reads `bbs.toml` and swaps it in — no restart, no dropped sessions:

```sh
sudo systemctl reload bbs-rs
```

bbs-rs also watches the file, so an edit in place is picked up on its own; the
explicit reload is for when you want it *now* and want the result in the journal.

**What a reload picks up:** branding, theme and art, `[features]`, `[limits]`,
`[abuse]`, `[accounts]`, `[files]`, `[oneliners]`, and the `[metrics] enabled`
toggle. **New** logins get the change; sessions already in progress keep the
settings they started with.

**What needs a restart:** the listeners — `[network]`, `[web]`, `[finger]`, and
`[metrics]` `host`/`port` — plus `host_key`, `database_url`, and `[seed]`. A
reload that touches those applies to the config but logs that a restart is
required for them to take effect.

**A broken config can't take the board down.** A `bbs.toml` that fails to parse
is rejected and the running config is kept:

```
ERROR bbs_rs::reload: config reload failed, keeping current settings:
  parsing config bbs.toml: TOML parse error at line 322, column 6
```

The server carries on serving with the last good config. Fix the file and reload
again.

---

## 5. Privileged ports

Three separate reasons you might want a port below 1024:

| Service | Port | Why |
|---|---|---|
| SSH | 22 | So users can type `ssh bbs.example.com` with no `-p`. |
| Web / federation | 443 | **Federation requires it** — [RFC 7565](https://www.rfc-editor.org/rfc/rfc7565) `acct:` URIs have no port component, so `@user@host:8088` isn't a valid handle. See [FEDERATION-SETUP.md](FEDERATION-SETUP.md). |
| finger | 79 | The RFC 1288 convention. |

Note that **port 22 usually already belongs to OpenSSH.** Either move sshd, or
put bbs-rs on another port, or give it a second IP address. Don't discover this
by taking your own administrative access offline.

Two ways to bind low ports:

**Grant the capability** (uncomment the two `Capabilit*` lines in the unit).
This is the smaller-blast-radius option: the process gets exactly one privilege
and keeps none of the rest of root.

**Or redirect** with your firewall and leave bbs-rs on high ports — often the
better choice, because the service then needs no privilege at all:

```sh
# nftables
sudo nft add rule inet nat prerouting tcp dport 22 redirect to :2222
```

If a reverse proxy already terminates TLS in front of the web frontend, set
`[web] tls = false` and let the proxy own 443. Don't do that for the metrics
endpoint — see [§7](#7-sandboxing-notes).

---

## 6. Secrets at rest

The state directory holds real secrets:

- **`host_key`** — the SSH host identity. Disclosure enables impersonating your board.
- **`bbs.db`** — argon2 password hashes, and, if federation is on, every local
  actor's **RSA private key**.

So: a dedicated unprivileged user, `0750` on the directory, and `UMask=0027` in
the unit so anything the server creates stays group-readable at most.

Back up with the online snapshot, which is safe against a running server — it
uses SQLite's `VACUUM INTO`, applies no migrations, and never writes to the live
database:

```sh
sudo -u bbs /usr/local/bin/bbsctl --config /var/lib/bbs-rs/bbs.toml \
    backup --out /var/backups/bbs-rs --files
```

`--files` also copies the file-area storage. **Add `host_key` yourself** —
`backup` doesn't take it, and §2 explains why you want it.

A timer, `/etc/systemd/system/bbs-rs-backup.service`:

```ini
[Unit]
Description=Snapshot the bbs-rs database
After=bbs-rs.service

[Service]
Type=oneshot
User=bbs
Group=bbs
WorkingDirectory=/var/lib/bbs-rs
ExecStart=/usr/local/bin/bbsctl --config /var/lib/bbs-rs/bbs.toml backup --out /var/backups/bbs-rs --files
ExecStart=/usr/bin/install -m 0600 /var/lib/bbs-rs/host_key /var/backups/bbs-rs/host_key
```

and `/etc/systemd/system/bbs-rs-backup.timer`:

```ini
[Unit]
Description=Nightly bbs-rs backup

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target
```

```sh
sudo install -d -o bbs -g bbs -m 0750 /var/backups/bbs-rs
sudo systemctl enable --now bbs-rs-backup.timer
```

Snapshots accumulate — the timer doesn't prune. Add rotation to taste.

---

## 7. Sandboxing notes

The directives in §3 are deliberately tight, but a few interact with things
bbs-rs actually does. Hardening that breaks federation or doors on first use is
worse than no hardening, so:

- **`ProtectSystem=strict` makes the whole filesystem read-only** except
  `ReadWritePaths=`. The state directory is listed. If you point
  `storage_dir`, `acme_cache`, or the database anywhere else, **add it too** —
  otherwise the first upload or certificate renewal fails.

- **Doors need more.** A door is an external program on a PTY
  ([`[[doors]]`](../README.md#configuration)). If you run any, the sandbox must
  permit them: add each door's `cwd` and binary path to `ReadWritePaths=` /
  keep them readable, and expect `MemoryDenyWriteExecute=true` and
  `SystemCallFilter=@system-service` to break interpreted or JIT-based doors.
  Loosen those two only if a door actually needs it, and prefer loosening for a
  reason you can name.

- **`PrivateDevices=true` still provides `/dev/pts`**, which doors need. It
  removes physical devices, not the pty subsystem.

- **Outbound network is required** if you enable ACME (Let's Encrypt) or
  federation (delivering activities, fetching remote actors). Don't add
  `IPAddressDeny=any` or `PrivateNetwork=true` without an allowlist — federation
  will fail in ways that look like remote-server problems.

- **`RestrictAddressFamilies`** keeps `AF_UNIX` deliberately: removing it breaks
  parts of the async runtime and resolver.

Check your work:

```sh
systemd-analyze security bbs-rs
```

Treat the score as a prompt, not a target. A tighter number that breaks
certificate renewal is a worse outcome.

---

## 8. Logs

Logs go to stderr and from there to the journal — never into a connected user's
terminal.

```sh
journalctl -u bbs-rs -f            # follow
journalctl -u bbs-rs -p err        # errors only
journalctl -u bbs-rs --since today
```

`Environment=RUST_LOG=info` is a reasonable default. `RUST_LOG=debug` is very
chatty (it includes every SQL statement) — useful when diagnosing, not something
to leave on. Per-module filters work too, e.g.
`RUST_LOG=info,bbs_rs::ssh=debug`.

---

## 9. Upgrading

Migrations run automatically at startup, so the short version is: replace the
binary and restart. To apply them ahead of time and keep the restart short — or
to see a failure before it becomes downtime:

```sh
sudo systemctl stop bbs-rs
sudo install -m 0755 bbs-rs bbsctl /usr/local/bin/
sudo -u bbs /usr/local/bin/bbs-rs --config /var/lib/bbs-rs/bbs.toml --migrate
sudo systemctl start bbs-rs
```

`--migrate` applies pending migrations and **exits without serving**. Take a
backup first (§6) — migrations are not reversible.

---

## 10. Troubleshooting

| Symptom | Likely cause |
|---|---|
| Users get an SSH **host key changed** warning | The service started from a different working directory, or `host_key` was lost. See §2. |
| The board is empty and users "vanished" | A second `bbs.db` was created somewhere else. `find / -name bbs.db` will find it; §2 prevents it. |
| `Permission denied` binding a port | Port below 1024 without `AmbientCapabilities=CAP_NET_BIND_SERVICE`, or something already holds it (§5). |
| Uploads or ACME fail with a read-only error | `ProtectSystem=strict` without the path in `ReadWritePaths=` (§7). |
| A reload "did nothing" | Either the setting is startup-bound (§4), or the file failed to parse — check the journal for `config reload failed`. |
| Federation is silent | Almost always the origin or port 443, not the sandbox. See [FEDERATION-SETUP.md](FEDERATION-SETUP.md). |

---

## See also

- [README](../README.md) — configuration reference and features
- [FEDERATION-SETUP.md](FEDERATION-SETUP.md) — turning on ActivityPub
- [ROADMAP.md](ROADMAP.md)
