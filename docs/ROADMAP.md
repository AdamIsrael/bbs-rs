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
| [**Unread / "new since last call"**](https://github.com/AdamIsrael/bbs-rs/issues/9) | M | Track last-seen per user/board and highlight new messages. | **Shipped**: `user_board_seen` watermark per user/board; the board list shows a green `(N new)` badge and unread posts are flagged (`•` + green) in the message list. Guests (shared account) are untracked. |
| [**Full-text search**](https://github.com/AdamIsrael/bbs-rs/issues/10) | M | Search boards (and maybe mail) by keyword. | **Shipped**: a `messages_fts` FTS5 index (trigger-synced) backs a **Search Messages** screen; results respect per-board read ACLs and Enter jumps to the message in its board. (Mail search not included.) |
| [**User profiles & signatures**](https://github.com/AdamIsrael/bbs-rs/issues/11) | M | Real name, location, tagline, signature, last-seen; shown on posts and a profile screen. | **Shipped**: `users` gains real_name/location/tagline/signature; a **My Profile** screen (with editor) shows those plus member-since, last-on (from `logins`), and post count; other users' profiles open from **Who's Online** (Enter); signatures render beneath board posts. |
| [**Stats / leaderboards / last callers**](https://github.com/AdamIsrael/bbs-rs/issues/12) | S | Top posters, call counter, recent callers list. | **Shipped**: a **Stats** screen shows totals (users/posts/calls), a top-posters leaderboard, and recent callers (most recent successful login per user), all aggregated over `users`/`messages`/`logins`. |

## Tier 3 — reach & extras

| Feature | Size | What & why | Depends on / status |
|---|---|---|---|
| [**WebSocket + xterm.js HTTP frontend**](https://github.com/AdamIsrael/bbs-rs/issues/13) | L | Reach the BBS from a browser, reusing the whole TUI. | **Shipped**: a `[web]` config toggle serves an axum HTTP server with a self-contained (vendored) xterm.js page and a `/ws` WebSocket. The web transport reuses the entire `app`/`input`/`Presence` stack — same auth (`attempt_login`), same session registry (who's-online + kick span SSH and web). |
| [**Door games / external programs**](https://github.com/AdamIsrael/bbs-rs/issues/14) | L | Launch classic door games or external programs per session. | **Shipped**: `[[doors]]` config launches programs on a PTY (portable-pty) over SSH **and** web; the TUI is suspended and raw bytes are bridged both ways, with user env vars, an optional `door.sys`/`dorinfo1.def` drop file, and a wall-clock time limit. |
| [**ANSI art menus & themes**](https://github.com/AdamIsrael/bbs-rs/issues/15) | M | Custom ANSI welcome screens and selectable color themes. | **Shipped**: a `[theme]` config section (built-in `classic`/`mono`/`amber`/`matrix` presets, plus per-color overrides in names/hex/256-index) drives all UI colors; `[art]` renders operator ANSI/text art (UTF-8 or CP437 `.ans`) as a main-menu welcome and optional per-screen headers. |
| [**New-mail notice**](https://github.com/AdamIsrael/bbs-rs/issues/16) | S | Show a user's unread private-mail count at login. | **Shipped**: a one-shot status-bar notice on login plus a persistent `(N new)` badge on the Private Mail menu row, both driven by `mail::unread_count` (mirrors the board `(N new)` pattern). Guests are untracked. The issue's RSS idea was dropped (no authenticated HTTP surface for a terminal BBS) and data export/backup shipped separately in [#60](https://github.com/AdamIsrael/bbs-rs/pull/60). |

---

## Cross-cutting notes

- [**Message/body length limits**](https://github.com/AdamIsrael/bbs-rs/issues/17) — **Shipped**:
  `[limits]` gains `max_subject_chars` (120) and `max_body_chars` (8000), enforced on board posts and
  mail (0 disables). Oneliners keep their own 120-char cap.
- [**Backups**](https://github.com/AdamIsrael/bbs-rs/issues/18) — **Shipped**: `bbsctl backup`
  snapshots the DB with SQLite's online `VACUUM INTO` (no downtime) to a timestamped file, and with
  `--files` also copies the file-area storage dir. Schedulable via cron/systemd.
- [**Seeded content in config**](https://github.com/AdamIsrael/bbs-rs/issues/19) — **Shipped**: a
  `[seed]` section defines the first-run boards (name/description/min_read/min_write, replacing the
  built-in General + Announcements) and the guest account's password. Boards seed only on a fresh DB.

---

# Roadmap v2 — beyond the basics

The original three tiers above are **all shipped**. This second wave targets the biggest remaining gaps:
real-time interaction, deeper messaging, account/operator lifecycle, and reach — plus three flagship
epics: **ActivityPub federation** (join the fediverse), an **operator-designable menu**, and
**context-aware templating**. Prioritization lens is unchanged (classic-BBS flavor + ops/security first).

## Theme A — Real-time & social *(classic flavor)*

The substrate already exists: every session parks a `Sender<Event>` in the shared `Presence` registry and
the per-session run loop redraws after every event, so pushed lines need no polling; SSH and web share the
same path. These add `Presence` fan-out methods (`send_to`, `send_to_user`, `broadcast`) + new `Event`
variants.

| Feature | Size | Issue |
|---|---|---|
| **Multi-user chat / teleconference** | M | [#67](https://github.com/AdamIsrael/bbs-rs/issues/67) |
| **User paging (yell)** | S | [#68](https://github.com/AdamIsrael/bbs-rs/issues/68) |
| **Sysop broadcast to live sessions** | S | [#69](https://github.com/AdamIsrael/bbs-rs/issues/69) |
| **User ignore / block list** — hide a user's posts, block their pages/mail | M | [#97](https://github.com/AdamIsrael/bbs-rs/issues/97) |

## Theme B — Deeper messaging *(engagement)*

| Feature | Size | Issue |
|---|---|---|
| **Mail actions: reply / forward / delete** | M | [#70](https://github.com/AdamIsrael/bbs-rs/issues/70) |
| **Mail to sysop** | S | [#71](https://github.com/AdamIsrael/bbs-rs/issues/71) |
| **Polls / voting booth** | M | [#72](https://github.com/AdamIsrael/bbs-rs/issues/72) |
| **Author edit/delete own posts** — today only admins can delete | M | [#92](https://github.com/AdamIsrael/bbs-rs/issues/92) |
| **Mail full-text search** — parity with board search (#10) | S | [#93](https://github.com/AdamIsrael/bbs-rs/issues/93) |
| **Post reactions / upvotes** | S | [#94](https://github.com/AdamIsrael/bbs-rs/issues/94) |
| **Attachments** — link file-area files to posts/mail | M | [#95](https://github.com/AdamIsrael/bbs-rs/issues/95) |
| **Full-screen multi-line compose editor** | M | [#96](https://github.com/AdamIsrael/bbs-rs/issues/96) |

## Theme C — Accounts, ops & security *(ops/security)*

| Feature | Size | Issue |
|---|---|---|
| **New-user validation queue** | M | [#73](https://github.com/AdamIsrael/bbs-rs/issues/73) |
| **Moderation / audit log** | M | [#74](https://github.com/AdamIsrael/bbs-rs/issues/74) |
| **Per-user daily time limits** | S | [#75](https://github.com/AdamIsrael/bbs-rs/issues/75) |
| **Password reset / account recovery (+ optional TOTP 2FA)** | M | [#76](https://github.com/AdamIsrael/bbs-rs/issues/76) |
| **Prometheus / metrics endpoint** | S | [#98](https://github.com/AdamIsrael/bbs-rs/issues/98) |
| **Sysop session monitor (spy)** — privacy-sensitive; needs a consent policy | M | [#99](https://github.com/AdamIsrael/bbs-rs/issues/99) |
| ~~**HTTPS/WSS for the web frontend**~~ | S | **Shipped** ([#54](https://github.com/AdamIsrael/bbs-rs/issues/54)): native TLS on by default when `[web]` is enabled — auto self-signed cert out of the box, operator-provided `tls_cert`/`tls_key`, or automatic Let's Encrypt via ACME (TLS-ALPN-01). The page already selects `wss://` from its origin. |

## Theme D — Reach *(classic flavor / reach)*

| Feature | Size | Issue |
|---|---|---|
| **`finger` service (RFC 1288)** — user discovery over TCP/79, reusing profiles + last-on + who's-online | S | [#77](https://github.com/AdamIsrael/bbs-rs/issues/77) |
| **Public-board RSS/Atom feeds** — viable now that the web server is an HTTP surface; guest-readable boards only | M | [#100](https://github.com/AdamIsrael/bbs-rs/issues/100) |
| **QWK offline-mail packets** — classic offline-reader support; orthogonal to federation (offline sneakernet, not server-to-server) | L | [#101](https://github.com/AdamIsrael/bbs-rs/issues/101) |
| **Email gateway (SMTP in/out)** — rescoped: ActivityPub covers notifications and cross-instance messaging, so only real-email bridging survives | L | [#103](https://github.com/AdamIsrael/bbs-rs/issues/103) |
| ~~**NNTP gateway**~~ | L | **Closed** ([#102](https://github.com/AdamIsrael/bbs-rs/issues/102)): superseded by Theme E — the same message-portability work a third time, through a new listener, for a vanishing audience. |

## Theme E — ActivityPub federation *(flagship, multi-phase)* — epic [#113](https://github.com/AdamIsrael/bbs-rs/issues/113)

Join the fediverse. Two goals, one protocol: **syndicate boards across bbs-rs instances**, and make
**users `user@host`** — followable from Mastodon, able to follow (and optionally message) people on other
platforms. Design of record: **[docs/FEDERATION.md](FEDERATION.md)**.

Goal 2 *requires* ActivityPub, and once actors/keys/signing/delivery exist for it, board syndication
reuses all of it — one implementation, both goals. This **supersedes the FidoNet epic** (#82, phases
#78–#81, all closed): that bought goal 1 only, and ~70% of its schema work was identical, so keeping both
meant paying for the same message-model surgery twice.

Prerequisites landed by accident: **#54** (native TLS + ACME — self-signed cannot interop) and **#88**
(`[web] hostname`). Note RFC 7565 `acct:` URIs have **no port component**, so `[web] port = 443` +
`acme_domains` is required for a valid origin.

| Phase | Size | Issue |
|---|---|---|
| **0 — relicense to AGPL-3.0 + design doc** *(the only maintained Rust AP crate is AGPL; the fediverse norm)* | S | [#114](https://github.com/AdamIsrael/bbs-rs/pull/114) |
| **1 — federated foundation** *(actors, keys, WebFinger, signing, durable queue)* | M | [#107](https://github.com/AdamIsrael/bbs-rs/issues/107) |
| **2 — outbound statuses** *(users followable from Mastodon; oneliners become `Note`s)* | L | [#108](https://github.com/AdamIsrael/bbs-rs/issues/108) |
| **3 — inbound** *(follows, remote statuses, content degradation)* | L | [#109](https://github.com/AdamIsrael/bbs-rs/issues/109) |
| **4 — remote DMs** *(opt-in, labeled not-private)* | M | [#110](https://github.com/AdamIsrael/bbs-rs/issues/110) |
| **5 — board syndication** *(bbs-rs ↔ bbs-rs, FEP-1b12 Groups)* | L | [#111](https://github.com/AdamIsrael/bbs-rs/issues/111) |
| **6 — inbound board posts + moderation** | L | [#112](https://github.com/AdamIsrael/bbs-rs/issues/112) |

Phase 2 leads because it pays off immediately against live Mastodon; board syndication needs a second
bbs-rs instance to exist before it means anything.

## Theme F — Operator-designable menu *(epic [#87](https://github.com/AdamIsrael/bbs-rs/issues/87))*

Let each operator design their main menu — placement, ANSI graphics, custom names/hotkeys, submenus.
Today the menu is a compile-time `MenuItem` enum with hardcoded labels/order (`src/app/state.rs`,
`src/app/mod.rs`) drawn as a bordered list; this moves structure and appearance into config.

| Phase | Size | Issue |
|---|---|---|
| **1 — config-driven labels, order & hotkeys** *(the action-dispatch foundation)* | M | [#84](https://github.com/AdamIsrael/bbs-rs/issues/84) |
| **2 — ANSI backdrop + item placement** | L | [#85](https://github.com/AdamIsrael/bbs-rs/issues/85) |
| **3 — submenus & custom targets** *(doors/boards/nested menus)* | M | [#86](https://github.com/AdamIsrael/bbs-rs/issues/86) |

**Shipped (all phases).** `[[menu]]` drives labels/order/hotkeys with feature and role gating; an
`[art] welcome` backdrop plus per-entry `row`/`col` renders the menu as a placed ANSI screen (falling
back to the list when incomplete or too small); and an entry's `action` can target a built-in, a
`door:<name>`, a `board:<name>`, or a `submenu:<name>` — the last pushing a `[[submenus.<name>]]` group
onto a menu stack so menus nest into a tree (Esc pops one level, capped against cycles). `bbsctl
validate-menu` reports unknown actions, dangling door/board/submenu targets, duplicate hotkeys, and
submenu cycles.

## Theme G — Context-aware templating *(epic [#91](https://github.com/AdamIsrael/bbs-rs/issues/91))*

Vary layout and content by runtime context — most notably the **connection method (SSH vs web)**, so a web
user can be told the SSH address and vice-versa. `App` is transport-agnostic today (`App::new` gets no
transport hint), so phase 1 is the prerequisite for the rest — and for the menu epic's dynamic content.

| Phase | Size | Issue |
|---|---|---|
| **1 — transport & session context in the UI** *(keystone; also ships the cross-advertising)* | S | [#88](https://github.com/AdamIsrael/bbs-rs/issues/88) |
| **2 — template engine for text content** *(variables + conditionals)* | M | [#89](https://github.com/AdamIsrael/bbs-rs/issues/89) |
| **3 — context-conditional ANSI/art & layout variants** | M | [#90](https://github.com/AdamIsrael/bbs-rs/issues/90) |
