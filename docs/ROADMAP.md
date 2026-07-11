# sshtui — feature roadmap

A prioritized catalog of common BBS features to evaluate, design, and build. This is a **planning
document**, not a commitment — it records what each feature is, its value, rough size, dependencies,
and what already exists in the codebase.

Prioritization lens (chosen): **classic-BBS flavor** and **operations/security** rank highest, then
user engagement, then broader reach.

Size key: **S** ≈ a session or two · **M** ≈ a few sessions · **L** ≈ multi-session / new subsystem.

## Already shipped

These are done and are the substrate several roadmap items build on:

- SSH-served ratatui TUI; guest + registered accounts (argon2); message boards (read/post);
  private mail; who's-online.
- Roles `guest` / `user` / `admin`; ban by username and IP; a login audit trail (`logins` table);
  live-session kicking via a ban sweeper; the `bbsctl` operator CLI.
- A TOML **configuration layer** (`bbs.toml`): branding (name/tagline/MOTD/sysop), network & SSH
  tunables, and feature toggles (registration / guest / private mail / who's-online).

---

## Tier 1 — next up (classic flavor + hardening)

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| **Bulletins / news** | S | A short list of sysop bulletins shown after login, beyond the single `bbs.welcome` MOTD. Classic "call of the day" feel. | MOTD already exists via config; add a `bulletins` table + a screen. |
| **Oneliners / graffiti wall** | S | A shared ring of short public messages users can append to and read. Iconic BBS feature, cheap to build. | New table + one screen; reuse the compose-form pattern. |
| **Board moderation & ACLs** | M | Admins/mods pin, lock, and delete posts; per-board read/write role requirements (e.g. an admin-only Announcements board). | Builds on the `admin` role and services layer; add a `min_role`/flags column to `boards`/`messages`. |
| **fail2ban / auto-ban** | M | Auto-ban an IP after N failed logins in a window; feed the existing ban sweeper. | **Substrate exists**: the `logins` table + `admin::ban_ip`. Just needs the detection policy. See the fail2ban note in project memory. |
| **Public-key SSH auth** | M | Let users register SSH public keys and authenticate with them; `auth_publickey` currently rejects all. | russh `auth_publickey`; a `user_keys` table. |
| **File areas** | L | Upload/download file areas with descriptions and quotas — a cornerstone of classic BBSes. | SFTP subsystem (russh) or an in-TUI transfer flow; storage + quota accounting. |
| **Rate limiting / post throttling** | S | Cap posts/mail per user per interval to blunt spam. | Services layer (boards/mail); pairs with auto-ban. |

## Tier 2 — engagement

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| **Message threading / replies** | M | Reply-to chains so boards read as conversations. | `messages.parent_id`; reader/threaded-list UI. |
| **Unread / "new since last call"** | M | Track last-seen per user/board and highlight new messages. | `user_board_seen` table; the `logins` table already gives "last call". |
| **Full-text search** | M | Search boards (and maybe mail) by keyword. | SQLite FTS5 virtual table over `messages`. |
| **User profiles & signatures** | M | Real name, location, tagline, signature, last-seen; shown on posts and a profile screen. | Extend `users`; a profile screen + editor. |
| **Stats / leaderboards / last callers** | S | Top posters, call counter, recent callers list. | Aggregations over `logins`/`messages`. |

## Tier 3 — reach & extras

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| **WebSocket + xterm.js HTTP frontend** | L | Reach the BBS from a browser, reusing the whole TUI. | The app is already transport-agnostic (byte-sink + `Event` contract) with a reserved `web` seam. |
| **Door games / external programs** | L | Launch classic door games or external programs per session. | Process spawning, drop-file/IO bridging, time limits. |
| **ANSI art menus & themes** | M | Custom ANSI welcome screens and selectable color themes. | Theme in config; an ANSI loader. |
| **Notifications / RSS / export** | M | New-mail notices, an RSS feed of a board, data export/backup. | Read-only projections over existing tables. |

---

## Cross-cutting notes

- **Message/body length limits** — none enforced today (posts/mail bodies are unbounded). Add
  configurable limits (`features`/a new `limits` section) alongside rate limiting.
- **Backups** — document/automate SQLite backup (`.backup` / file copy while quiesced).
- **Seeded content in config** — default boards and the guest account are still hardcoded; a future
  `[seed]` config section could make them operator-defined (explicitly deferred).
