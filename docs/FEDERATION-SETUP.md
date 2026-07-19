# Enabling federation — operator guide

This is the hands-on guide to turning on ActivityPub federation for a bbs-rs
board: what you must have first, how to configure it, the exact steps to connect
with Mastodon, and how to syndicate boards between instances. For the *why* and
the design, see
[FEDERATION.md](FEDERATION.md).

> **Read this first: the domain is permanent.** An ActivityPub actor URI (e.g.
> `https://bbs.example.com/u/alice`) is a primary key across the entire network.
> Once your board has delivered anything to a remote server, that URI can never
> change. **Changing your federation domain later orphans every remote follow,
> every delivered post, and every DM.** Decide the domain before you enable
> federation, and treat it as forever.

---

## 1. Hard requirements

Federation cannot work without all of these. bbs-rs validates them **fail-closed
at startup**: if the origin is wrong, the board refuses to federate rather than
mint permanent broken URIs.

| Requirement | Why |
|---|---|
| **A fixed, public domain name** (`bbs.example.com`) | Actor URIs and WebFinger `acct:` handles are built on it, permanently. Not an IP address, not `localhost`. |
| **Reachable on port 443** | [RFC 7565](https://www.rfc-editor.org/rfc/rfc7565) `acct:` URIs have **no port component**, so `@user@host:8088` is not a valid fediverse handle and WebFinger discovery would fail. The origin must be plain `https://host`. |
| **A CA-trusted TLS certificate** | Remote servers fetch you with ordinary HTTPS clients and default trust stores. A self-signed cert cannot interop. Let's Encrypt (built in) or a real cert from a reverse proxy. |
| **Public DNS** pointing the domain at your server | So Mastodon and friends can resolve and reach you (and, for ACME, so Let's Encrypt can validate you). |
| **The web frontend enabled** (`[web] enabled = true`) | All ActivityPub endpoints (WebFinger, actors, inbox) are served by the browser frontend. SSH-only boards can't federate. |

The origin you configure must be **scheme + host only** — no port, no path, no
query. Valid: `https://bbs.example.com`. Rejected: `https://bbs.example.com:8088`,
`http://…` (unless `debug_insecure`, see §8), `https://1.2.3.4`,
`https://localhost`, `https://host/bbs`.

---

## 2. Two ways to serve on 443

The origin must be `https://yourdomain` with no port, so the frontend has to
answer on 443. Pick one:

### Option A — bbs-rs terminates TLS directly (Let's Encrypt)

bbs-rs fetches and renews a trusted cert itself over ACME (TLS-ALPN-01). Simplest
if the box has nothing else on 443.

```toml
[web]
enabled = true
host    = "0.0.0.0"
port    = 443
tls     = true
acme_domains = ["bbs.example.com"]
acme_email   = "sysop@example.com"
acme_cache   = "acme-cache"        # persists the account key + issued certs
# acme_staging = true              # test the ACME flow first (untrusted certs)
```

Binding port 443 needs privilege — run under a service manager that grants it, or
`sudo setcap 'cap_net_bind_service=+ep' /path/to/bbs-rs`. Port 443 must be open
to the internet for both federation traffic and the ACME challenge.

> Test the ACME handshake with `acme_staging = true` first — staging has far
> higher rate limits, so a misconfiguration doesn't burn your weekly quota.
> Switch to `false` once a staging cert is issued cleanly.

### Option B — a reverse proxy terminates TLS

If nginx/Caddy/etc. already owns 443, let it handle TLS and forward to bbs-rs on
a high port. The origin is still the public domain (no port); bbs-rs doesn't need
to know it's proxied.

```toml
[web]
enabled  = true
host     = "127.0.0.1"
port     = 8088
tls      = false          # the proxy does TLS
hostname = "bbs.example.com"
```

The proxy must forward these paths to `127.0.0.1:8088` (forwarding `/` is
simplest): `/.well-known/webfinger`, `/.well-known/nodeinfo`, `/nodeinfo/2.1`,
`/u/…`, `/s/…`, `/c/…` (board Groups), and `/inbox`. Preserve the `Host` header
and the request path.

Either way, `[federation] origin` is the same:

```toml
[federation]
enabled = true
origin  = "https://bbs.example.com"
```

---

## 3. Minimal working config

A complete example (Option A, direct ACME):

```toml
[web]
enabled = true
host    = "0.0.0.0"
port    = 443
tls     = true
acme_domains = ["bbs.example.com"]
acme_email   = "sysop@example.com"

[federation]
enabled = true
origin  = "https://bbs.example.com"
# allowlist_only = true      # default — see §5
# allow_remote_dms = false   # default — see §5d
# delivery_interval_secs = 30
# delivery_max_attempts = 10
```

Everything under `[federation]` besides `enabled`/`origin` has a safe default;
you can start with just those two.

---

## 4. Start it and verify

`[federation]` is **restart-only** — changes are read at startup, not hot-reloaded
(a reload logs a "restart to take effect" warning). Restart the server after
editing federation config.

On a good startup you'll see:

```
web frontend listening on https://0.0.0.0:443
ActivityPub federation enabled
```

If the origin is invalid you'll instead get a startup error naming the exact
problem (bad scheme, a port, an IP, localhost, a path). Fix it and restart.

Then confirm the surface answers, from any machine:

```bash
# WebFinger resolves a local user to their actor
curl -s 'https://bbs.example.com/.well-known/webfinger?resource=acct:alice@bbs.example.com'

# The actor document (Person) — this also lazily mints alice's keypair
curl -s -H 'Accept: application/activity+json' https://bbs.example.com/u/alice

# Instance metadata
curl -s https://bbs.example.com/nodeinfo/2.1
```

`alice` must be a **registered, non-guest** account (the shared `guest` account
never federates). WebFinger returning the actor URI, and the actor document
carrying a `publicKey`, means you're discoverable.

---

## 5. Connect with Mastodon

### 5a. Allow the peer (default posture)

bbs-rs ships **allowlist-only** (`allowlist_only = true`): a small board doesn't
want to moderate the entire internet, so it federates only with domains you name.
Allow the instances you want to interact with:

```bash
bbsctl ap-allow mastodon.social
bbsctl ap-allow fosstodon.org "a friend's instance"   # optional reason
bbsctl ap-peers                                        # list allow/block entries
```

Allow every remote domain you'll follow, be followed from, or DM. (To federate
openly instead, set `allowlist_only = false` and use `ap-block` for bad actors —
but then you're on the hook for moderating anyone.)

`bbsctl` operates on the same database the server uses, so allow/block entries
take effect immediately — no restart needed for policy changes (only for
`[federation]` config itself).

### 5b. Be followed from Mastodon

1. In Mastodon's search, enter `@alice@bbs.example.com`.
2. Mastodon WebFingers your domain, fetches the actor, and shows the account.
3. Follow it. Mastodon POSTs a `Follow`; bbs-rs replies `Accept`.
4. In bbs-rs, `alice` posts an **oneliner** (the Oneliners menu). It's delivered
   to that follower's inbox and appears in their Mastodon timeline.

### 5c. Follow a Mastodon account from bbs-rs

1. Open the **Timeline** screen (main menu; appears when federation is on).
2. Press `f`, enter a handle like `Mastodon@mastodon.social`, Enter.
3. bbs-rs resolves it over WebFinger and sends a signed `Follow` (it shows as
   *pending* until they accept — which is automatic for most accounts).
4. Their public posts arrive at your inbox, get degraded from HTML to plain text
   (links kept, images shown as `[img: alt]`), and show on the Timeline.

Sysops can also drive this from the CLI:

```bash
bbsctl ap-follow alice Mastodon@mastodon.social
bbsctl ap-following alice          # list who alice follows, with state
bbsctl ap-unfollow alice Mastodon@mastodon.social
```

### 5d. (Optional) remote direct messages

Off by default, and **deliberately** — fediverse DMs are not private (they sit in
plaintext on every server they pass through). To allow them:

```toml
[federation]
allow_remote_dms = true
```

Then addressing private mail to a `user@host` recipient sends a Mastodon-style
direct message; the compose screen labels it, in bold, as leaving the BBS and not
being private. Incoming fediverse DMs land in the mailbox tagged
`[fedi · not private]`. Local mail is always private and unaffected.

---

## 6. Syndicating boards (bbs-rs ↔ bbs-rs)

Every board is also a **`Group` actor**
([FEP-1b12](https://codeberg.org/fediverse/fep/src/branch/main/fep/1b12/fep-1b12.md)),
so boards can be followed and shared between instances — and with Lemmy/Mbin,
which speak the same shape.

Each board gets a URI-safe **slug** derived from its name (assigned at startup
when federation is on), and lives at `https://your.host/c/{slug}` with the
handle `@{slug}@your.host`. Check yours:

```bash
curl -s -H 'Accept: application/activity+json' https://bbs.example.com/c/general
curl -s 'https://bbs.example.com/.well-known/webfinger?resource=acct:general@bbs.example.com'
```

### Letting others follow your boards

Nothing to configure — it works once federation is on. A remote instance follows
`@general@bbs.example.com`; your board auto-accepts (`manuallyApprovesFollowers:
false`), and from then on every **top-level** post is `Announce`d to that
subscriber, signed by the board and attributed to its author. Replies don't
syndicate yet.

Remember the peer's domain still has to be allowed (§5a) in the default
allowlist posture.

### Subscribing to a remote board

Subscribing *is* following the board's Group, so the same command does it:

```bash
bbsctl ap-allow peer.example              # allow the peer first
bbsctl ap-follow alice general@peer.example
bbsctl ap-board-posts general@peer.example   # cached posts from that board
```

Announced posts arrive at your inbox, are degraded from HTML to plain text, and
are cached locally. `ap-board-posts` also accepts the board's actor URI
(`https://peer.example/c/general`) if the handle doesn't resolve.

### Accepting posts from other instances

Your boards also **accept posts from remote instances** — nothing to configure.
A post whose `audience` names one of your board Groups is filed on that board,
threaded under its parent if it's a reply, and then re-`Announce`d from your
board to every subscriber: your instance is that board's hub, so it's what
propagates the post onward. The original author's attribution is preserved, not
rewritten to your domain.

Two properties worth knowing, because they're what keep this safe:

- **Routing is by `audience`, not by which inbox the post arrived at.** A reply
  from Mastodon is delivered to a *person's* inbox rather than the board's; it
  still reaches the board. (This is the failure mode where Lemmy loses Mastodon
  replies.)
- **Remote HTML is never stored or rendered.** Everything is flattened to plain
  text on the way in — that flattening *is* the sanitization, since the
  federation library performs none of its own. A remote author is also held to
  the same `[limits] max_posts` per-window budget as a local one, so one peer
  can't flood a board even if its own server doesn't rate-limit.

> **Still missing**, so you know where the edges are: posting from *here* into a
> followed remote board; a browsable in-BBS screen for mirrored boards (they're
> an operator-visible cache via `bbsctl` today); and remote `Delete`/`Update`
> handling plus the moderation surface (inbound reports, domain-block severity).
> Those are the remaining federation work.

---

## 7. `bbsctl` federation commands

| Command | What it does |
|---|---|
| `ap-peers` | List allow/block domains |
| `ap-allow <domain> [reason]` | Permit a domain (needed in the default allowlist posture) |
| `ap-block <domain> [reason]` | Block a domain (used when `allowlist_only = false`) |
| `ap-unallow <domain>` / `ap-unblock <domain>` | Remove an allow/block entry |
| `ap-follow <user> <name@host>` | Follow a remote account on a local user's behalf |
| `ap-unfollow <user> <name@host>` | Unfollow |
| `ap-following <user>` | List the remote accounts a user follows, with follow state |
| `ap-board-posts <board>` | Show cached posts from a followed remote board (handle or actor URI) |

Point `bbsctl` at the same config the server uses: `bbsctl --config /path/to/bbs.toml <cmd>`.

---

## 8. Testing locally without a public domain

To exercise the machinery on your workstation — or to test bbs-rs ↔ bbs-rs — use
**`debug_insecure = true`**, which relaxes the origin rules to permit `http://`,
`localhost`, and explicit ports so two instances can federate on one machine.

```toml
[web]
enabled = true
host    = "127.0.0.1"
port    = 8093
tls     = false

[federation]
enabled        = true
origin         = "http://localhost:8093"
debug_insecure = true
allow_remote_dms = true       # if you're testing DMs
```

Run a second instance on different ports (`8094`, its own database), allow each
other's host (`bbsctl --config a.toml ap-allow localhost`), and you can follow,
post, and DM between them locally.

> **Never set `debug_insecure = true` on a real board.** It lets you mint
> `http://localhost` URIs, which are permanent garbage the moment they leave the
> box. It exists only for local testing.

---

## 9. Troubleshooting

| Symptom | Likely cause |
|---|---|
| Startup error `[federation] origin must use https` / `must not include a port` / `must be a domain name` | The origin isn't scheme+host-only on a real domain. Fix per the message; see §1. |
| `ActivityPub federation enabled` never logged | `[federation] enabled` is false, or `[web] enabled` is false (the endpoints live on the web frontend). |
| Mastodon can't find `@alice@yourdomain` | WebFinger unreachable (check §4 curl), the domain in the handle doesn't match `origin`'s host, or `alice` is the guest / an unregistered name. |
| A follow or post never arrives | The peer's domain isn't allowed (`bbsctl ap-peers`), or the cert isn't CA-trusted (self-signed can't interop), or 443 isn't publicly reachable. |
| A remote instance's posts never land on my board | The peer's domain isn't allowed, the post carries no `audience` naming your board, its author isn't the signer, or that author hit the `[limits] max_posts` inbound cap. |
| A followed board's posts never appear | The follow isn't `accepted` yet (`bbsctl ap-following <user>`), the peer's domain isn't allowed, or the post was a **reply** — only top-level posts syndicate. |
| Config change had no effect | `[federation]` is restart-only — restart the server. (Allow/block entries via `bbsctl` are the exception and apply immediately.) |
| ACME cert never issues | Port 443 not reachable from the internet, DNS not pointing at the box, or you hit the rate limit — retry with `acme_staging = true`. |

Delivery is a durable, backing-off queue, so transient remote outages retry on
their own; a post isn't lost if a peer is briefly down.
