# bbs-rs — feature roadmap

A prioritized catalog of common BBS features to evaluate, design, and build. This is a **planning
document**, not a commitment — it records what each feature is, its value, rough size, dependencies,
and what already exists in the codebase.

Each item links to its GitHub issue; the [roadmap tracking issue](https://github.com/AdamIsrael/bbs-rs/issues/20)
has the overall checklist.

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
| [**Bulletins / news**](https://github.com/AdamIsrael/bbs-rs/issues/1) | S | A short list of sysop bulletins shown after login, beyond the single `bbs.welcome` MOTD. Classic "call of the day" feel. | MOTD already exists via config; add a `bulletins` table + a screen. |
| [**Oneliners / graffiti wall**](https://github.com/AdamIsrael/bbs-rs/issues/2) | S | A shared ring of short public messages users can append to and read. Iconic BBS feature, cheap to build. | **Shipped**: `oneliners` table + screen, `features.oneliners` toggle, `bbsctl oneliners`/`rm-oneliner`. |
| [**Board moderation & ACLs**](https://github.com/AdamIsrael/bbs-rs/issues/3) | M | Admins/mods pin, lock, and delete posts; per-board read/write role requirements (e.g. an admin-only Announcements board). | **Shipped**: `boards.min_read_role`/`min_write_role`/`locked` + `messages.pinned`; admin keys (pin/delete/lock) in the TUI; `bbsctl boards`/`set-board`. |
| [**fail2ban / auto-ban**](https://github.com/AdamIsrael/bbs-rs/issues/4) | M | Auto-ban an IP after N failed logins in a window; feed the existing ban sweeper. | **Substrate exists**: the `logins` table + `admin::ban_ip`. Just needs the detection policy. |
| [**Public-key SSH auth**](https://github.com/AdamIsrael/bbs-rs/issues/5) | M | Let users register SSH public keys and authenticate with them; `auth_publickey` currently rejects all. | **Shipped**: `user_keys` table + `auth_publickey`/`_offered` (SHA256 fingerprint match); in-BBS **SSH Keys** screen, `bbsctl keys`/`add-key`/`rm-key`, `features.pubkey_auth` toggle. |
| [**File areas**](https://github.com/AdamIsrael/bbs-rs/issues/6) | L | Upload/download file areas with descriptions and quotas — a cornerstone of classic BBSes. | **Shipped**: `file_areas`/`files` schema, in-BBS browser, `[files]` quota + extension limits, FS storage, `bbsctl` management, **SFTP upload/download** (areas as a virtual filesystem) with ACL/quota/type enforcement ([#38](https://github.com/AdamIsrael/bbs-rs/issues/38)), and an **in-BBS viewer** for text files + zip/tar.gz/gz archives ([#41](https://github.com/AdamIsrael/bbs-rs/issues/41)). |
| [**Rate limiting / post throttling**](https://github.com/AdamIsrael/bbs-rs/issues/7) | S | Cap posts/mail per user per interval to blunt spam. | **Shipped**: `[limits]` config throttles posts/mail/oneliners per user per window (admins exempt); enforced in the services layer, no new table. |

## Tier 2 — engagement

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| [**Message threading / replies**](https://github.com/AdamIsrael/bbs-rs/issues/8) | M | Reply-to chains so boards read as conversations. | **Shipped**: `messages.parent_id`; `r` to reply (auto `Re:`); the message list renders threads depth-first with indentation. |
| [**Unread / "new since last call"**](https://github.com/AdamIsrael/bbs-rs/issues/9) | M | Track last-seen per user/board and highlight new messages. | `user_board_seen` table; the `logins` table already gives "last call". |
| [**Full-text search**](https://github.com/AdamIsrael/bbs-rs/issues/10) | M | Search boards (and maybe mail) by keyword. | SQLite FTS5 virtual table over `messages`. |
| [**User profiles & signatures**](https://github.com/AdamIsrael/bbs-rs/issues/11) | M | Real name, location, tagline, signature, last-seen; shown on posts and a profile screen. | Extend `users`; a profile screen + editor. |
| [**Stats / leaderboards / last callers**](https://github.com/AdamIsrael/bbs-rs/issues/12) | S | Top posters, call counter, recent callers list. | Aggregations over `logins`/`messages`. |

## Tier 3 — reach & extras

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| [**WebSocket + xterm.js HTTP frontend**](https://github.com/AdamIsrael/bbs-rs/issues/13) | L | Reach the BBS from a browser, reusing the whole TUI. | The app is already transport-agnostic (byte-sink + `Event` contract) with a reserved `web` seam. |
| [**Door games / external programs**](https://github.com/AdamIsrael/bbs-rs/issues/14) | L | Launch classic door games or external programs per session. | Process spawning, drop-file/IO bridging, time limits. |
| [**ANSI art menus & themes**](https://github.com/AdamIsrael/bbs-rs/issues/15) | M | Custom ANSI welcome screens and selectable color themes. | Theme in config; an ANSI loader. |
| [**Notifications / RSS / export**](https://github.com/AdamIsrael/bbs-rs/issues/16) | M | New-mail notices, an RSS feed of a board, data export/backup. | Read-only projections over existing tables. |

---

## Cross-cutting notes

- [**Message/body length limits**](https://github.com/AdamIsrael/bbs-rs/issues/17) — none enforced
  today (posts/mail bodies are unbounded). Add configurable limits (`features`/a new `limits`
  section) alongside rate limiting.
- [**Backups**](https://github.com/AdamIsrael/bbs-rs/issues/18) — document/automate SQLite backup
  (`.backup` / file copy while quiesced).
- [**Seeded content in config**](https://github.com/AdamIsrael/bbs-rs/issues/19) — default boards and
  the guest account are still hardcoded; a future `[seed]` config section could make them
  operator-defined.
