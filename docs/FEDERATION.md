# Federation — ActivityPub design

Design doc for making bbs-rs a fediverse server. This is a **plan of record**, not shipped behavior —
see the epic and its phase issues for status.

> **Operators:** to turn federation on, see the [operator guide](FEDERATION-SETUP.md) — hard requirements,
> config, and the steps to connect with Mastodon. This document is the design rationale behind it.

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

Phase 6 (#112) is sliced:

- **112a — inbound board posts** (this slice): a remote `Create` whose `audience` (or `to`/`cc`) names one
  of our board Groups is accepted onto that board. **Routing is by `audience`, not by which inbox it was
  delivered to** — that's the Lemmy weakness this project set out to beat: a Mastodon reply lands in a
  *person's* inbox, and on Lemmy it never propagates; here it still reaches the board. The post's author
  must be the signer, content is degraded to plain text on the way in (**that degradation is also our
  sanitization** — no remote HTML is ever stored or rendered, and the crate does none of its own), storage
  dedups on `ap_id`, `inReplyTo` threads a reply under its parent, and a remote author gets the same
  per-window post budget as a local one. We then **re-`Announce` as the Group** — we're the board's home, so
  subscribers hear it from us, with the original author's attribution preserved.
- **112b — remote lifecycle** (this slice): honor `Delete` and `Update` for content we've accepted, across
  all four federated stores — board posts (`messages`), cached statuses (`ap_timeline`), mirrored board
  posts (`ap_board_posts`), and inbound DMs (`mail`). **Authorization lives in the SQL**: every statement's
  `WHERE` requires the acting actor to own the row, so one actor can never touch another's content and an
  unknown object is indistinguishable from someone else's. A mirrored post may also be withdrawn by the
  board that announced it (in FEP-1b12 the Group is the authority for its own content), which is why
  migration 0017 adds `ap_board_posts.author_uri` — a display handle isn't an identity to authorize
  against. Deleting a board post also drops it from the FTS index, since the 0012 triggers fire.
  `Undo` was already handled for `Follow`, the only `Undo` we act on; `Announce`-wrapped lifecycle
  (a Group relaying a member's `Delete`) came later, in #133.
- **112c — moderation surface** (this slice): inbound `Flag` (remote reports) recorded for operators —
  **never acted on automatically**, since auto-acting would hand any peer a remote moderation lever over
  this board. Domain blocks gain **severity**: `suspend` is the hard block we already had (refused at the
  door by the `UrlVerifier`), while `silence` still lets a domain federate — existing follows and opted-in
  DMs keep working — but stops its content entering shared surfaces (boards, timeline, mirrors). Because
  defederation is **not retroactive**, `bbsctl ap-purge <domain> --yes` is the explicit second step for
  content that already arrived; it's deliberately separate so deleting content is never a silent side
  effect of a policy change. Operator surface: `ap-reports`, `ap-resolve`, `ap-block --severity`,
  `ap-purge`.

### #131 — posting into a followed remote board (post-epic follow-up)

The receiving half has worked since #112a; this is the sending half. The asymmetry with our own boards is
the point: when we *host* a board we `Announce` from the Group because we're the hub, but here we're a
contributor, so the **author** signs a plain `Create{Page}` addressed `to: [group]`, `cc: [Public]`,
`audience: group`.

**A submission is not a mirrored post, and not a local post either**, which is why migration 0020 adds a
third table (`ap_outbox_posts`). `messages` is our boards — filing it there would put it on a local board
it isn't on. `ap_board_posts` is the mirror of foreign objects — writing it there would assert the remote
board had published something it hasn't. The new table also supplies the post's permanent `ap_id`, minted
from its row id.

So a submission is shown as **awaiting the board** until that board announces it back, at which point the
announced copy lands in `ap_board_posts` under the same `ap_id` and supersedes it. `pending()` is an
anti-join against the mirror rather than a status column, so nothing has to be kept in sync — publication
is observed, not recorded. Optimistically showing it as published would be asserting something only the
remote board can say.

#### The bug this uncovered

The two-instance run failed at first with the author's instance returning 500 and logging
`Activity was sent from local instance`. Cause: `board_announce` minted the `Announce`'s id by deriving it
from the **post's** URI. That was invisible while every announced post originated on the announcing
instance — the derived id was on the right domain by accident. Once a board announces a post authored
*elsewhere* (which #112a introduced, and #131 exercises), the activity id lands under the author's domain,
and the author's own server correctly rejects it as spoofing its domain.

The effect was that **the one instance guaranteed to care about a post was the one instance that could
never receive the announcement** — and it failed silently, as a delivery-queue error nobody reads. An
activity's id must belong to whoever created it; the id is now minted from our own origin and the local
row id. Same fix for `Announce{Delete}`. Pinned by `an_announce_of_a_remote_post_is_still_our_activity`.

**Replies are still out of scope**, deliberately: our own boards don't syndicate replies yet either
(#111b), so shipping outbound replies alone would make BBS↔BBS threading half-work in one direction.

### #132 — in-BBS screen for mirrored remote boards (post-epic follow-up)

Mirrored posts were reachable only through `bbsctl ap-board-posts`, so board syndication was invisible
to the users it's for. **Remote Boards** on the main menu lists subscribed boards and their mirrored
posts.

**A sibling screen, not a reuse of the board screens.** Mirrored posts live in `ap_board_posts`, outside
`messages`, because they're foreign objects we cache rather than content we're the authority for.
Rendering them through the local board UI would blur exactly the line that design decision draws — so the
screen is separate, its titles say "mirrored", and it offers no post or moderate action.

**What the work actually turned on: nothing recorded that an actor was a `Group`.** Remote `Person`s and
`Group`s are both `users` rows, which was fine while both were only ever reached *by* their actor URI —
but "which of my follows are boards?" had no answer. Migration 0019 adds `users.actor_kind`, filled from
the fetched actor document (which is why `Person::kind` is a lenient `String`).

Two deliberate choices there:

- **Nullable, not defaulted to `'Person'`.** A default would be a guess about rows we never recorded a
  type for, and a wrong guess silently hides a board from its own screen. NULL honestly means "unknown".
- **Backfill on evidence, not assumption.** The migration marks as `Group` only those actors that have
  actually announced a board post to us. For anything it can't prove, `mirror::boards` falls back to the
  same evidence at query time, so a board followed before the upgrade doesn't vanish.

Follow state is surfaced (`pending` vs `accepted`) because a board awaiting the remote server's `Accept`
is legitimately empty, and an unexplained empty screen reads as a bug. Where several local users follow
the same board in different states, the query takes `MIN(state)` — mirroring is instance-wide, so if any
edge is accepted the board is live for everyone; without an aggregate SQLite would return an arbitrary
row's state, which is a coin flip in precisely the case the screen is trying to explain.

### #133 — `Announce`-wrapped lifecycle (post-epic follow-up)

A board relays its members' `Delete`/`Update` so a post withdrawn upstream doesn't linger in every
subscriber's mirror. Both halves: we send `Announce{Delete}` when a syndicated board post is deleted
here, and we honor one arriving from a board we subscribe to.

**The design question is authorization, not parsing.** A relayed activity is signed by the *Group*, so
the signature alone proves only that the board sent it — not that the board had any standing to. Taking
the Group's word for it would let any board we follow withdraw anything it could name. So a relayed
lifecycle op is deliberately much narrower than a direct one:

- it can only reach `ap_board_posts` — never `messages`, `ap_timeline`, or `mail`. Content on **our**
  boards is ours; a remote board doesn't get to moderate it, and an attempt is logged at `warn` rather
  than passing silently;
- the row must have been announced by **that** Group (`group_uri` matches), so a board can only act on
  what it actually hosts; and
- the inner activity's actor must be the post's author or the Group itself — a board vouches for its
  members, not for anyone who asks.

The outbound half has an ordering constraint worth naming: everything the `Announce{Delete}` needs (the
post's `ap_id`, its board, its author) lives in the row about to be deleted, so the activity is *built*
first and *queued* only after the local delete succeeds. The reverse order would tell subscribers to drop
a post we then failed to remove ourselves. Hence `outbound::prepare_board_delete` + `dispatch` rather
than one call.

Both authorization guards are covered by mutation-checked tests: removing either the host-scope or the
inner-actor condition makes a specific test fail.

Phase 5 (#111) is sliced:

- **111a — boards as `Group` actors** (this slice): the read/discovery surface. Each board lazily mints a
  URI-safe **slug** (from its name, collision-suffixed) + a Group keypair, minted eagerly at startup since
  a Group's slug is *derived*, not a natural key like a username. Served at `/c/{slug}` as a FEP-1b12
  `Group` (`manuallyApprovesFollowers: false`), with a WebFinger handle (`acct:slug@host`) and an outbox of
  the board's root posts as `Announce{Create{Page}}` — a root post is a `Page` (`name` = subject),
  attributed to its author's `Person`, `audience` = the Group. No delivery or inbound yet.
- **111b — Group follow + `Announce` fan-out**: a remote instance `Follow`s the Group (→ `ap_follows` +
  `Accept`); a local board post is wrapped and `Announce`d from the Group to its followers.
- **111c — inbound `Announce` → local mirror** (this slice): following a remote board is just following its
  Group (`ap-follow <user> <slug@host>`); the remote `Group` is fetched through the same actor path as a
  Person (its `type` deserializes leniently). When a followed board `Announce`s a post, the `Page` is
  degraded to text and cached in `ap_board_posts` (migration 0016), gated on following that Group and
  idempotent on the Page's id — `bbsctl ap-board-posts` lists them. A board Group's inbox is served at
  `/c/{slug}/inbox`. **Verified end-to-end between two bbs-rs instances**: A follows B's board, B posts, A
  mirrors it — each step signed and verified (B's Group signature and A's user signature).

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
