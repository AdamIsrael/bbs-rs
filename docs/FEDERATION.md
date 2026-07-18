# Federation — ActivityPub design

Design doc for making bbs-rs a fediverse server. This is a **plan of record**, not shipped behavior —
see the epic and its phase issues for status.

## Goals

1. **Syndicate boards across bbs-rs instances.** bbs-rs ↔ bbs-rs. A board on one board appears and
   accepts posts on another.
2. **Users become `user@host`.** Followable from Mastodon and friends, and able to follow (and
   optionally message) people on other platforms.

## Why ActivityPub

Goal 2 *requires* it — there is no other way to be followable from Mastodon. And once actors, keypairs,
signing, and a delivery queue exist for goal 2, goal 1 reuses all of it. **One implementation, both
goals.**

This is why AP **replaces** the FidoNet-style federation epic ([#82](https://github.com/AdamIsrael/bbs-rs/issues/82),
phases #78–#81), which was closed as superseded. That design would have bought goal 1 only, and ~70% of
its schema work (global message ids, remote authors, portable board ids, dedup) is identical to AP's —
so doing both would have meant paying for the same message-model surgery twice, including the risky
`messages` rebuild, for a private network instead of a public one.

**Lemmy is a model, not a compatibility target.** Its inline images are content a terminal BBS can't
render, and chasing its feature set would drag us somewhere we can't go. But boards still adopt Lemmy's
[FEP-1b12](https://codeberg.org/fediverse/fep/src/branch/main/fep/1b12/fep-1b12.md) `Group` shape (a
FINAL spec; the established answer for exactly this problem): it's the same work as inventing our own
profile, it's standards-compliant, and Lemmy/Mbin *can* federate with us as a free bonus.

## Prerequisites (already met)

AP needs a **public HTTPS origin with a CA-valid certificate** — self-signed cannot interop, since peers
use ordinary HTTPS clients with default trust stores. [#54](https://github.com/AdamIsrael/bbs-rs/issues/54)
(native TLS + ACME) and [#88](https://github.com/AdamIsrael/bbs-rs/issues/88) (`[web] hostname`) landed
before this was planned and happen to supply exactly that.

**The port matters.** [RFC 7565](https://www.rfc-editor.org/rfc/rfc7565.txt) `acct:` URIs have **no port
component** — `acct:user@bbs.example.com:8088` is not a valid acct URI, and WebFinger discovery is built
on them. So the default `:8088` can never be a federation endpoint. `[web] port = 443` + `acme_domains`
is a valid origin **with no reverse proxy needed**.

## The shape

| BBS concept | ActivityPub | Phase |
|---|---|---|
| Local user | `Person` actor | 1–2 |
| Status (oneliners, reworked) | `Note`, public | 2 |
| Instance | `Application` actor (for signed fetch) | 1 |
| Board | `Group` actor (FEP-1b12) | 5 |
| Root post (`parent_id IS NULL`) | `Page` (`name` = subject) | 5 |
| Reply (`parent_id` set) | `Note` + `inReplyTo` | 5 |
| Private mail | local by default; remote = opt-in, labeled | 4 |
| `guest` | **does not federate** | — |

### What deliberately does not federate

**`guest`.** AP identity is one actor = one keypair = one accountable entity. A shared account means
shared reputation: the first abuse report suspends every guest at once. WebFinger also requires
`preferredUsername` be unique per domain, so `guest@host` would be one actor for everyone. Guests stay
read-only and local.

**Private mail, by default.** Fediverse DMs are **not encrypted** — they sit in plaintext in the database
of every server they touch, readable by any admin along the path (Lemmy shipped a private-message leak
advisory for exactly this). BBS users reasonably believe mail is private; silently turning it into a
plaintext copy on a stranger's server would be a user-harm footgun. Remote addressing is therefore an
explicit opt-in, visibly labeled in the UI as leaving the BBS, and kept distinct from local mail.

## Statuses = oneliners, reworked

Each oneliner becomes a `Note` attributed to its author. A user's oneliners are their **outbox**; the
wall becomes the instance's **local timeline**. Following a bbs-rs user from Mastodon shows their
statuses.

- **The ring buffer must go.** `[oneliners] max_entries` auto-trims the wall — but a federated post has a
  permanent URI, and trimming would orphan remote references and require `Delete` fan-out. This
  deliberately reverts [#32](https://github.com/AdamIsrael/bbs-rs/issues/32). The wall now grows forever;
  moderation and explicit deletion replace auto-trim.
- **The length cap is raised, not removed** — 120 → **500**, matching Mastodon. "Like a federated post"
  means a *server-defined* limit, not none: unbounded statuses are an abuse vector and remote servers
  reject oversized payloads. `0` still means unlimited for operators who want it.
- `[limits] max_oneliners` (rate limiting) is unaffected, and matters more now that the ring buffer is
  gone — it's what keeps the wall sane.
- **Naming is unresolved:** "oneliners" stops being accurate once they aren't one line.

Wire details that fail **silently** rather than loudly, so each is pinned by a test:

- the full `https://www.w3.org/ns/activitystreams#Public` URI, never the `as:Public` CURIE;
- camelCase throughout (`preferredUsername`, `publicKeyPem`, `sharedInbox`);
- RFC 3339 `published`;
- status bodies HTML-escaped into their `<p>` wrapper — AP `content` is HTML, and a body must not be able
  to inject markup into every reader's timeline.

## Key design decision: additive-only schema

The obvious approach — make `messages.author_id` nullable so remote authors can exist — requires a full
SQLite table rebuild. `messages` is the content table for the `messages_fts` FTS5 **external-content**
index (migration 0012), so rebuilding it means dropping and recreating the virtual table *and* all three
sync triggers in the same migration. That is the single riskiest migration available to us.

**We avoid it.** Remote actors become rows in `users`, keyed by a **fully-qualified
`alice@remote.social` username** — which is how fediverse handles are displayed anyway. Consequently:

- `messages.author_id NOT NULL REFERENCES users(id)` is **untouched**. No rebuild. FTS undisturbed.
- `users.username UNIQUE` is **untouched** — the domain lives inside the handle, so it naturally prevents
  duplicate actors across domains.
- Every change is `ALTER TABLE ... ADD COLUMN` + `CREATE UNIQUE INDEX` on new nullable URI columns
  (SQLite treats NULLs as distinct, so existing local rows are fine).

This is safe because the FTS triggers reference `new.id` / `new.subject` / `new.body` **explicitly**, so
adding unrelated columns to `messages` cannot disturb the index. **A migration test must prove this** —
it is the load-bearing assumption of the whole approach.

### The costs, accepted knowingly

- **Every user-listing query must filter `is_remote = 0`** — admin users, stats/leaderboards,
  who's-online, mail recipient lookup. Miss one and remote actors leak into local UI.
- `users.password_hash` is `NOT NULL`, so shadow rows carry an unusable sentinel. Argon2 verification
  against it always fails, so it is safe — but auth must reject `is_remote = 1` **explicitly** rather
  than relying on that.
- **Registration must reject `@` in usernames.** This is a live hole today: validation only blocks exact
  reserved names (`guest`/`root`/`admin`), so a local user can currently register `alice@remote.social`
  and impersonate a remote actor. A prerequisite, not a polish item.

## Phases

| # | Scope | Size | Status |
|---|---|---|---|
| 0 | Relicense to AGPL-3.0, this doc, issue housekeeping | S | **done** (#114) |
| 1 | Federated foundation: keypairs, `users` AP columns, WebFinger, `Person`, `[federation]` config | M | **done** (#115) |
| 2 | **Outbound statuses** — oneliners rework, `Note`, outbox, nodeinfo, delivery-queue storage. No inbox | M–L | **done** (#116) |
| 3 | **Inbound** — inbox POST, signature verification, `Follow`/`Accept`, remote statuses, timeline screen, allowlist, **content degradation**, queue drain | L | #109 (in progress) |

Phase 3 (#109) is sliced into reviewable PRs:

- **A — inbound plumbing + signature verification** (**done**, #117): the inbox as a POST endpoint, the
  `Object`/`Actor` impls that let the crate fetch a sender's key, HTTP-signature verification via
  `receive_activity`, remote actors persisted as `is_remote` shadow rows, and the domain **allowlist**
  (`ap_blocks` + `bbsctl ap-*`, enforced through the crate's `UrlVerifier`). Signatures are *verified* but
  activities are not yet *acted on* — a permissive `AnyActivity` accepts-and-logs.
- **B — Follow/Accept + the queue drain** (**done**, #118): typed `Follow`/`Undo` activities behind an
  `InboundActivity` enum (the `AnyActivity` catch-all stays as the fallback arm). An inbound `Follow` of a
  local user is recorded in `ap_follows` and answered with a queued `Accept`; an `Undo{Follow}` removes it.
  The delivery queue's **drain** — the sign-and-POST loop deferred from phase 2 — is spawned at startup
  like `ban_sweeper`, using the crate's `SendActivityTask` so we don't hand-roll HTTP signatures. Posting a
  status now fans a `Create{Note}` out to the author's follower inboxes. This is where a user becomes
  *followable* from real Mastodon. The `Note`/`Create{Note}` wire shape moved to
  `services::federation::objects` so the read surface and the delivery fan-out can't drift.
- **C1 — inbound `Create{Note}` ingestion + content degradation** (this slice): a `Create` arm on
  `InboundActivity` caches a remote status in `ap_timeline` (migration 0014) — but only if a *local* user
  follows the author (`follows::is_followed_locally`) **and** the Note's `attributedTo` is the actor who
  signed the delivery (no third-party injection). `content::html_to_text` degrades the HTML at ingestion:
  `<p>`/`<br>` → lines, `<a>` keeps its text and appends the URL when it adds anything, `<img>` →
  `[img: alt] (src)`, entities decoded, other tags stripped. Storage is idempotent on the Note's `ap_id`.
  This is the ingestion engine; nothing yet *creates* the outbound follows that make statuses arrive, so
  (like Slice A before B) it's inert in production until C2 wires it up.
- **C2 — outbound follow + `Accept` handling + the timeline screen**: following a remote account
  (`ap_object::follow` — WebFinger-resolve the handle, mint the local actor, store a `pending` edge, queue a
  signed `Follow`), the inbound `Accept` arm that flips the edge to `accepted` (guarded so only the followed
  account can accept), `Undo{Follow}` to unfollow, and a read-only **Timeline** TUI screen that shows the
  cached `ap_timeline` statuses. Follow/unfollow shipped through `bbsctl ap-follow` / `ap-unfollow` /
  `ap-following`.
- **C3 — in-BBS follow**: the Timeline screen's `f` key opens a "follow `user@host`" prompt that calls the
  same `follow` path. (C2 deferred this on the belief that the app calling `web` — where the
  `FederationConfig` builder lives — would be a dependency cycle. That was wrong: bbs-rs is a single crate,
  and Rust allows mutual `use` between its modules, so `app` calls `web::ap_object::follow_handle`
  directly. No refactor was needed; the "move the trait impls into `services`" follow-up is moot.)
| 4 | Remote DMs — opt-in, labeled not-private | M | #110 |
| 5 | **Board syndication** (bbs-rs ↔ bbs-rs) — `Group` actors + `Announce` fan-out | L | #111 |
| 6 | Inbound board posts + moderation | L | #112 |

Phase 5 (#111) is sliced:

- **111a — boards as `Group` actors** (this slice): the read/discovery surface. Each board lazily mints a
  URI-safe **slug** (from its name, collision-suffixed) + a Group keypair, minted eagerly at startup since
  a Group's slug is *derived*, not a natural key like a username. Served at `/c/{slug}` as a FEP-1b12
  `Group` (`manuallyApprovesFollowers: false`), with a WebFinger handle (`acct:slug@host`) and an outbox of
  the board's root posts as `Announce{Create{Page}}` — a root post is a `Page` (`name` = subject),
  attributed to its author's `Person`, `audience` = the Group. No delivery or inbound yet.
- **111b — Group follow + `Announce` fan-out**: a remote instance `Follow`s the Group (→ `ap_follows` +
  `Accept`); a local board post is wrapped and `Announce`d from the Group to its followers.
- **111c — inbound `Announce` → local mirror**: follow a remote board, receive its `Announce`d posts into a
  mirrored local board.

Phase 4 (#110) is sliced:

- **110a — outbound remote DM** (this slice): a `[federation] allow_remote_dms` opt-in (**off by default**);
  addressing mail to a `user@host` recipient sends a Mastodon-compatible **direct** `Create{Note}` —
  `to: [actor]`, `cc: []`, and a matching `Mention` in `tag` (without the Mention, Mastodon treats it as
  *limited*, not direct). The compose screen labels a remote recipient as leaving the BBS and not private;
  a local copy is recorded (`mail` row to the recipient's shadow actor). Local mail is untouched and stays
  private.
- **110b — inbound remote DM** (this slice): a direct `Create{Note}` addressed to one of our local actors
  (in `to`/`cc`, and **not** the Public collection) lands in that user's mailbox instead of the timeline —
  behind the same `allow_remote_dms` opt-in (off → dropped silently). The content degrades to text; the
  `summary` (if any) becomes the subject, else "Direct message"; storage is idempotent on the Note's
  `ap_id` (migration 0015 adds `mail.ap_id`). The mailbox and reader label these as non-private fediverse
  messages. Note the subject round-trips imperfectly BBS↔BBS: 110a encodes the subject as a **bold first
  line** in `content` (not `summary`, which is Mastodon's content-warning field), so a receiving bbs-rs
  recovers it as body text, not a separate subject — a deliberate Mastodon-friendly trade.

Phase 2 leads because it pays off against live Mastodon sooner than board syndication, which needs a
second bbs-rs instance to exist before it means anything.

### What phase 2 does *not* deliver

**Being followed requires an inbox.** Mastodon POSTs a `Follow` and expects an `Accept`; with no inbox
that POST 404s and the follow hangs pending. Delivery has the same gap — no followers means nothing to
deliver to. So after phase 2 a user is **discoverable and fetchable** (WebFinger → actor → outbox), not
followable. Following, delivery, and the queue's drain loop all arrive together in **phase 3**, which is
the first point any of them has a real target.

This is why phase 2's delivery work is storage-only: `enqueue`/`due`/`mark_*`/backoff are fully specified
by the schema and testable in isolation, but signing and POSTing needs somewhere to POST *to*.

### Content degradation (phase 3) is unavoidable

Rejecting Lemmy as a target does *not* escape rich content — it relocates it. Mastodon statuses are HTML
with images, custom emoji, and media attachments. Following Mastodon accounts means receiving all of it.
The layer (HTML → text, image → `[img: alt]` + URL, optionally OSC-8 hyperlinks on the web frontend) is
mandatory, not cosmetic. Note the AP crate explicitly performs **no** sanitization of received data.

## Implementation notes

**Dependency:** `activitypub_federation = { version = "=0.7.0-beta.11", default-features = false,
features = ["axum"] }` — LemmyNet's crate, the only maintained Rust option.

- **Pin exactly.** axum 0.8 support exists *only* in the 0.7 beta series (stable 0.6.5 needs axum 0.7),
  which has been in beta ~a year with trait renames still landing (`ActivityHandler` → `Activity`).
- Default features pull in actix-web; disable them.
- It is **storage-agnostic** (trait-based: `Object`, `Actor`, `Activity`, `Collection`, with `Data<T>`
  carrying our `SqlitePool`), so sqlx/SQLite is a non-issue.
- It gives us HTTP Signatures both directions, actor fetching/caching, inbox dispatch, WebFinger, and
  AS2 kinds. It does **not** give us persistence, sanitization, nodeinfo, or a blocklist (only a
  `url_verifier` hook).
- **Its delivery queue is in-memory**, retrying at ~1min / 1hr / 2.5 days — a restart silently drops
  deliveries. Hence our own durable SQLite queue in phase 1.
- It brings the **first outbound HTTP client into the codebase** (`reqwest`); `web::probe_health`
  currently hand-rolls HTTP/1.0 over a raw `TcpStream` specifically to avoid that dependency.

**Actor keypairs** are RSA-2048 with the private half stored — the **first secret-at-rest besides
`password_hash`**. They will flow into `bbsctl backup` (`VACUUM INTO`) and `services::archive` exports.
That needs to be a conscious decision, not a discovery.

**Config:** a new `[federation]` section (`enabled`, `origin`, allowlist mode, key settings). **Do not
derive the origin from `Web::connect_url()`** — it can return `https://localhost:8088`, and AP `id` URIs
are permanent primary keys across the network that can never be rewritten once delivered. Validate
**fail-closed** at startup (https + real domain + CA cert) and refuse to enable federation otherwise.
`[federation]` is startup-bound like `[web]`, so it needs `PartialEq` + an entry in
`reload::warn_restart_only`.

## Moderation floor

A federating instance must ship at least: **allowlist-by-default** (for a small BBS this is a feature,
not a limitation — open federation means volunteering to moderate the entire internet), domain blocks
with severity, HTTP signature verification rejecting unsigned/bad-sig, inbound `Flag` handling, rate
limiting plus a backoff queue, registration gating, and honoring remote `Delete`/`Update`/`Undo`. Note
that defederation is **not retroactive**: dropping a peer stops updates, it does not delete what already
arrived.

## Risks

- **This is a subsystem, not a feature.** Lemmy's federation code is ~15–25k LOC. Ours is smaller
  (statuses + boards, no rich media), but phases 3, 5, and 6 are each an **L**.
- **AGPL and the beta pin are one-way doors.**
- **Federation is permanent.** Actor URIs and the domain can never change without orphaning every remote
  follow. Decide the domain before the first delivery.
- **Mastodon is strict** — authorized-fetch (signed GETs), signature quirks, and the `as:Public`
  CURIE-vs-full-URI interop bug (emit the full URI).
- **Mastodon ↔ Lemmy interop is lossy**, and we inherit that shape: Mastodon replies go to personal
  inboxes rather than the Group, so they never get `Announce`d and don't propagate. We can beat this by
  accepting activities that carry `audience` pointing at one of our boards and re-announcing them
  ourselves.
- **Board syndication's payoff needs peers.** A federated BBS with no peers is just a BBS with extra
  HTTP.

## Verify before coding

The field shapes above come from prose docs and FEPs; verbatim Lemmy/Mastodon actor JSON could not be
fetched during research (asset paths 404'd). **Confirm against a live instance** before implementing:

```sh
curl -H 'Accept: application/activity+json' https://lemmy.ml/c/announcements
curl -H 'Accept: application/activity+json' https://mastodon.social/users/Mastodon
```
