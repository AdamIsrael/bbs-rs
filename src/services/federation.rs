//! ActivityPub actor identity: URI minting and keypairs (epic #113, #107).
//!
//! Two rules govern everything here.
//!
//! **URIs are permanent.** An ActivityPub `id` is a primary key across the
//! whole network. Once an actor or object has been delivered to a remote
//! server, its URI can never be rewritten — so URIs are only ever minted from
//! an origin that [`crate::config::Federation::origin`] has already validated,
//! and once a row has an `actor_uri` we never recompute it.
//!
//! **Local and remote actors share `users`.** A discovered remote actor is a
//! row keyed by a fully-qualified `alice@remote.social` handle with
//! `is_remote = 1` (see docs/FEDERATION.md). Registration rejects `@`, so the
//! two namespaces cannot collide.

use activitypub_federation::http_signatures::generate_actor_keypair;
use sqlx::sqlite::SqlitePool;

use crate::db::models::User;
use crate::error::{AppError, Result};

/// The validated public origin, and the URI layout built on it.
///
/// Construct via [`Origin::new`], which only accepts an origin that
/// [`crate::config::Federation::origin`] has approved — that's what keeps a
/// `https://localhost:8088` from ever reaching a minted URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin(String);

impl Origin {
    /// Build from an already-validated origin string.
    pub fn new(validated: impl Into<String>) -> Self {
        Self(validated.into())
    }

    /// Resolve straight from config, validating fail-closed.
    pub fn from_config(fed: &crate::config::Federation) -> anyhow::Result<Self> {
        Ok(Self::new(fed.origin()?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host part, as used in `acct:user@host` and WebFinger.
    pub fn host(&self) -> &str {
        self.0
            .split_once("//")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.0)
            .split('/')
            .next()
            .unwrap_or_default()
    }

    /// A local user's `Person` actor.
    pub fn person(&self, username: &str) -> String {
        format!("{}/u/{username}", self.0)
    }

    pub fn person_inbox(&self, username: &str) -> String {
        format!("{}/u/{username}/inbox", self.0)
    }

    pub fn person_outbox(&self, username: &str) -> String {
        format!("{}/u/{username}/outbox", self.0)
    }

    pub fn person_followers(&self, username: &str) -> String {
        format!("{}/u/{username}/followers", self.0)
    }

    /// The instance-wide shared inbox. Remote servers deliver one copy here for
    /// many local recipients instead of fanning out per-actor.
    pub fn shared_inbox(&self) -> String {
        format!("{}/inbox", self.0)
    }

    /// A board's `Group` actor (#111). Keyed by slug: `boards.name` is free
    /// text and not URI-safe.
    pub fn group(&self, slug: &str) -> String {
        format!("{}/c/{slug}", self.0)
    }

    pub fn group_inbox(&self, slug: &str) -> String {
        format!("{}/c/{slug}/inbox", self.0)
    }

    pub fn group_outbox(&self, slug: &str) -> String {
        format!("{}/c/{slug}/outbox", self.0)
    }

    pub fn group_followers(&self, slug: &str) -> String {
        format!("{}/c/{slug}/followers", self.0)
    }

    /// A status (oneliner) `Note`.
    pub fn status(&self, id: i64) -> String {
        format!("{}/s/{id}", self.0)
    }

    /// A board post (`Page` for a root, `Note` for a reply).
    pub fn post(&self, id: i64) -> String {
        format!("{}/p/{id}", self.0)
    }

    /// A private-message `Note` (#110). Its own path so a DM is never confused
    /// with a public status.
    pub fn dm(&self, id: i64) -> String {
        format!("{}/dm/{id}", self.0)
    }

    /// The `Create` activity that wraps a status in an outbox.
    pub fn status_activity(&self, id: i64) -> String {
        format!("{}/s/{id}/activity", self.0)
    }

    /// The `acct:` URI a WebFinger query resolves, e.g. `acct:alice@bbs.example.com`.
    pub fn acct(&self, username: &str) -> String {
        format!("acct:{username}@{}", self.host())
    }
}

/// A local actor's stored identity.
#[derive(Debug, Clone)]
pub struct ActorKeys {
    pub actor_uri: String,
    pub public_key: String,
    pub private_key: String,
}

/// Fetch a local user's actor identity, generating and storing it on first use.
///
/// Generation is lazy rather than at registration: a board that never enables
/// federation should never hold RSA private keys, and the origin isn't known to
/// be valid until federation is switched on.
///
/// Idempotent — if a row already has an `actor_uri` we return it untouched.
/// Re-minting a URI would orphan every remote follow of that actor.
pub async fn ensure_person_keys(
    pool: &SqlitePool,
    origin: &Origin,
    user: &User,
) -> Result<ActorKeys> {
    if user.is_remote {
        // Remote actors' keys are theirs; we only ever store their *public*
        // half, fetched from their server.
        return Err(AppError::NotFound);
    }

    let existing: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT actor_uri, public_key, private_key FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(pool)
            .await?;

    if let Some((Some(actor_uri), Some(public_key), Some(private_key))) = existing {
        return Ok(ActorKeys {
            actor_uri,
            public_key,
            private_key,
        });
    }

    let keypair = generate_actor_keypair()
        .map_err(|e| AppError::Other(anyhow::anyhow!("generating actor keypair: {e}")))?;
    let actor_uri = origin.person(&user.username);
    let inbox = origin.person_inbox(&user.username);
    let shared_inbox = origin.shared_inbox();

    sqlx::query(
        "UPDATE users SET actor_uri = ?, inbox_url = ?, shared_inbox_url = ?, \
         public_key = ?, private_key = ? WHERE id = ?",
    )
    .bind(&actor_uri)
    .bind(&inbox)
    .bind(&shared_inbox)
    .bind(&keypair.public_key)
    .bind(&keypair.private_key)
    .bind(user.id)
    .execute(pool)
    .await?;

    tracing::info!("minted ActivityPub actor {actor_uri} for {}", user.username);
    Ok(ActorKeys {
        actor_uri,
        public_key: keypair.public_key,
        private_key: keypair.private_key,
    })
}

/// A board's stored `Group` identity (#111).
#[derive(Debug, Clone)]
pub struct GroupKeys {
    pub actor_uri: String,
    pub slug: String,
    pub public_key: String,
    pub private_key: String,
}

/// A URI-safe slug from a board's free-text name: lowercase, non-alphanumerics
/// collapsed to single `-`, trimmed. Empty results (a name of only symbols) fall
/// back to `board-{id}` — the caller passes the board id for that.
pub fn slugify(name: &str, board_id: i64) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("board-{board_id}")
    } else {
        slug.to_string()
    }
}

/// Fetch a board's `Group` identity, generating slug + keypair on first use.
///
/// Lazy and idempotent, exactly like [`ensure_person_keys`]: a board that never
/// federates mints nothing, and once a Group has a slug/URI they are permanent
/// (re-slugging would orphan every remote subscriber). Slug collisions get a
/// `-{id}` suffix so each board's Group URI is unique.
pub async fn ensure_group_keys(
    pool: &SqlitePool,
    origin: &Origin,
    board_id: i64,
) -> Result<GroupKeys> {
    let (name, slug, actor_uri, public_key, private_key): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT name, slug, actor_uri, public_key, private_key FROM boards WHERE id = ?",
    )
    .bind(board_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    if let (Some(slug), Some(actor_uri), Some(public_key), Some(private_key)) =
        (slug.clone(), actor_uri, public_key, private_key)
    {
        return Ok(GroupKeys {
            actor_uri,
            slug,
            public_key,
            private_key,
        });
    }

    // Pick a slug (keep any existing one), avoiding collisions.
    let slug = match slug {
        Some(s) => s,
        None => {
            let base = slugify(&name, board_id);
            let taken: bool =
                sqlx::query_scalar("SELECT COUNT(*) FROM boards WHERE slug = ? AND id != ?")
                    .bind(&base)
                    .bind(board_id)
                    .fetch_one(pool)
                    .await
                    .map(|n: i64| n > 0)?;
            if taken {
                format!("{base}-{board_id}")
            } else {
                base
            }
        }
    };

    let keypair = generate_actor_keypair()
        .map_err(|e| AppError::Other(anyhow::anyhow!("generating group keypair: {e}")))?;
    let actor_uri = origin.group(&slug);
    sqlx::query(
        "UPDATE boards SET slug = ?, actor_uri = ?, public_key = ?, private_key = ? WHERE id = ?",
    )
    .bind(&slug)
    .bind(&actor_uri)
    .bind(&keypair.public_key)
    .bind(&keypair.private_key)
    .bind(board_id)
    .execute(pool)
    .await?;

    tracing::info!("minted ActivityPub Group {actor_uri} for board {board_id}");
    Ok(GroupKeys {
        actor_uri,
        slug,
        public_key: keypair.public_key,
        private_key: keypair.private_key,
    })
}

/// Resolve a board by its Group actor URI — how an inbound post addressed to
/// `audience` (or `to`) finds its board (#112).
pub async fn find_board_by_actor_uri(pool: &SqlitePool, uri: &str) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM boards WHERE actor_uri = ?")
            .bind(uri)
            .fetch_optional(pool)
            .await?,
    )
}

/// Resolve a board by its Group slug — the `boards.id` behind `/c/{slug}`.
pub async fn find_board_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM boards WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?)
}

/// Mint Group slugs + keypairs for every board that lacks them.
///
/// Unlike a `Person` (fetched at its natural `/u/{username}`), a Group lives at
/// `/c/{slug}` where the slug is *derived* from the board name — so a board must
/// have its slug assigned before it's discoverable at all. Run once at startup
/// when federation is enabled; boards are few and operator-created.
pub async fn ensure_all_group_keys(pool: &SqlitePool, origin: &Origin) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM boards WHERE slug IS NULL")
        .fetch_all(pool)
        .await?;
    for id in ids {
        ensure_group_keys(pool, origin, id).await?;
    }
    Ok(())
}

/// Record a board message's permanent `ap_id`, minting it on first use. Lazy,
/// like [`ensure_status_ap_id`] — a board that never federates mints no URIs.
pub async fn ensure_message_ap_id(pool: &SqlitePool, origin: &Origin, id: i64) -> Result<String> {
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT ap_id FROM messages WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    match existing {
        None => Err(AppError::NotFound),
        Some(Some(ap_id)) => Ok(ap_id),
        Some(None) => {
            let ap_id = origin.post(id);
            sqlx::query("UPDATE messages SET ap_id = ? WHERE id = ?")
                .bind(&ap_id)
                .bind(id)
                .execute(pool)
                .await?;
            Ok(ap_id)
        }
    }
}

/// The ActivityStreams "public" collection. **Emit the full URI, not the
/// `as:Public` CURIE** — the short form is a known interop bug that makes posts
/// invisible on some servers.
pub const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// The ActivityStreams `@context` every object and activity carries on the wire.
pub const AS_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";

/// The `Note`/`Create{Note}` wire shapes for a status.
///
/// Single source of truth: the read surface (`GET /s/{id}`, the outbox) and the
/// outbound delivery fan-out (#109) both build statuses here, so a change to the
/// wire shape can't drift between what we publish and what we deliver.
pub mod objects {
    use super::*;
    use crate::db::models::Oneliner;
    use serde::Serialize;

    /// A status: one oneliner, as an ActivityStreams `Note`.
    // ActivityStreams is camelCase on the wire; getting a field name wrong
    // doesn't error, it silently fails to interop.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Note {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub attributed_to: String,
        pub content: String,
        pub published: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
    }

    /// A `Create` activity wrapping a `Note`, as delivered and as it appears in
    /// an outbox.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CreateNote {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub published: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub object: Note,
    }

    /// Minimal HTML escaping for status bodies. Statuses are plain text, but AP
    /// content is HTML, so a body must not be able to inject markup into every
    /// reader's timeline.
    pub fn html_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    /// Build the `Note` for a status, minting its permanent `ap_id` on first use.
    pub async fn note_for(pool: &SqlitePool, origin: &Origin, o: &Oneliner) -> Result<Note> {
        let ap_id = ensure_status_ap_id(pool, origin, o.id).await?;
        Ok(Note {
            context: AS_CONTEXT,
            kind: "Note",
            id: ap_id,
            attributed_to: origin.person(&o.author_name),
            content: format!("<p>{}</p>", html_escape(&o.body)),
            published: crate::util::fmt_rfc3339(o.created_at),
            // The full Public URI, never the `as:Public` CURIE.
            to: vec![PUBLIC.to_string()],
            cc: vec![origin.person_followers(&o.author_name)],
        })
    }

    /// Build the `Create{Note}` for a status — the outbox item and the delivered
    /// activity are the same object.
    pub async fn create_for(
        pool: &SqlitePool,
        origin: &Origin,
        o: &Oneliner,
    ) -> Result<CreateNote> {
        let note = note_for(pool, origin, o).await?;
        Ok(CreateNote {
            context: AS_CONTEXT,
            kind: "Create",
            id: origin.status_activity(o.id),
            actor: origin.person(&o.author_name),
            published: note.published.clone(),
            to: note.to.clone(),
            cc: note.cc.clone(),
            object: note,
        })
    }

    /// A `Mention` tag. Mastodon derives *direct* visibility from every
    /// addressed actor also appearing as a Mention in `tag` — omit it and the
    /// message is treated as *limited*, not a DM. (#110)
    #[derive(Debug, Clone, Serialize)]
    pub struct Mention {
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub href: String,
        pub name: String,
    }

    /// A private message as an ActivityStreams `Note`: addressed to one actor,
    /// `cc` empty, and that actor mentioned in `tag`.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DirectNote {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub attributed_to: String,
        pub content: String,
        pub published: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub tag: Vec<Mention>,
    }

    /// A `Create` wrapping a [`DirectNote`], as delivered to the recipient's inbox.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DirectCreate {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub object: DirectNote,
    }

    /// Build the `Create{Note}` for a private message from a local user to a
    /// remote actor. `note_id` is this message's permanent URI (minted by the
    /// caller from the stored mail row). A non-empty subject becomes a bold
    /// first line — fediverse Notes have no subject field.
    pub fn direct_message(
        note_id: &str,
        sender_actor: &str,
        recipient_actor: &str,
        recipient_handle: &str,
        subject: &str,
        body: &str,
        published_unix: i64,
    ) -> DirectCreate {
        let subject = subject.trim();
        let content = if subject.is_empty() {
            format!("<p>{}</p>", html_escape(body))
        } else {
            format!(
                "<p><b>{}</b></p><p>{}</p>",
                html_escape(subject),
                html_escape(body)
            )
        };
        let note = DirectNote {
            context: AS_CONTEXT,
            kind: "Note",
            id: note_id.to_string(),
            attributed_to: sender_actor.to_string(),
            content,
            published: crate::util::fmt_rfc3339(published_unix),
            // Direct: addressed to the one recipient, never Public or followers.
            to: vec![recipient_actor.to_string()],
            cc: Vec::new(),
            tag: vec![Mention {
                kind: "Mention",
                href: recipient_actor.to_string(),
                name: format!("@{}", recipient_handle.trim_start_matches('@')),
            }],
        };
        DirectCreate {
            context: AS_CONTEXT,
            kind: "Create",
            id: format!("{note_id}/activity"),
            actor: sender_actor.to_string(),
            to: note.to.clone(),
            cc: note.cc.clone(),
            object: note,
        }
    }

    // ---- Boards as Group actors (FEP-1b12, #111) --------------------------

    /// One item on a board: a root post or a reply.
    ///
    /// The two differ only in `kind` and `inReplyTo` — a root is a `Page`, a
    /// reply is a `Note` carrying the parent's URI (#139). They share a struct
    /// because they share all their addressing, and the alternative (a `Page`
    /// type that sometimes emits `"type": "Note"`) is exactly the sort of thing
    /// that misleads whoever reads it next.
    ///
    /// **`name` is kept on replies too**, which is unusual: AP `Note`s
    /// conventionally have no name, but a BBS reply has a real subject and
    /// dropping it would lose information on every bbs-rs ↔ bbs-rs hop. It's
    /// valid AS2, Mastodon ignores it, and a peer that understands it keeps full
    /// fidelity. Receivers must not *depend* on it — ours falls back to
    /// `Re: <parent>`.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BoardItem {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub attributed_to: String,
        pub name: String,
        pub content: String,
        pub published: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        /// The board this belongs to — FEP-1b12 uses `audience` for the Group.
        pub audience: String,
        /// Set only on replies: the parent's permanent URI.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub in_reply_to: Option<String>,
    }

    /// A `Create` wrapping a board `Page`, authored by the poster.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CreatePage {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub audience: String,
        pub object: BoardItem,
    }

    /// The Group's `Announce` of a post to its followers — the FEP-1b12 fan-out.
    /// The Group (not the author) is the actor; the `Create` rides embedded.
    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Announce {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub object: CreatePage,
    }

    /// Build the `Note` for a board **reply** (#139).
    ///
    /// Identical to [`board_page`] but for `kind` and `inReplyTo`, which points
    /// at the parent's permanent URI. That parent URI may be ours or a remote
    /// peer's — a thread on a board we host can contain posts from several
    /// instances, and a reply always points at whatever the parent's real id is.
    #[allow(clippy::too_many_arguments)]
    pub fn board_reply(
        origin: &Origin,
        slug: &str,
        ap_id: &str,
        author_uri: &str,
        subject: &str,
        body: &str,
        published_unix: i64,
        in_reply_to: &str,
    ) -> BoardItem {
        let mut item = board_page(
            origin,
            slug,
            ap_id,
            author_uri,
            subject,
            body,
            published_unix,
        );
        item.kind = "Note";
        item.in_reply_to = Some(in_reply_to.to_string());
        item
    }

    /// Build the `Page` for a board root post. `ap_id` is its permanent URI
    /// (minted by the caller); `author_uri` is the poster's `Person`.
    #[allow(clippy::too_many_arguments)]
    pub fn board_page(
        origin: &Origin,
        slug: &str,
        ap_id: &str,
        author_uri: &str,
        subject: &str,
        body: &str,
        published_unix: i64,
    ) -> BoardItem {
        BoardItem {
            context: AS_CONTEXT,
            kind: "Page",
            id: ap_id.to_string(),
            attributed_to: author_uri.to_string(),
            name: subject.to_string(),
            content: format!("<p>{}</p>", html_escape(body)),
            published: crate::util::fmt_rfc3339(published_unix),
            to: vec![PUBLIC.to_string()],
            cc: vec![origin.group_followers(slug)],
            audience: origin.group(slug),
            in_reply_to: None,
        }
    }

    /// Build a `Page` addressed **to a remote board** (#131).
    ///
    /// Mirror image of [`board_page`], which addresses one of *our* Groups: here
    /// the audience is someone else's Group, and the primary recipient is that
    /// Group rather than the public. `to: [group]` + `cc: [Public]` is the
    /// FEP-1b12 addressing a Lemmy-style board expects for a submission; our own
    /// inbound routing reads `audience` first and falls back to to/cc, so a
    /// bbs-rs peer accepts it either way.
    pub fn remote_board_page(
        ap_id: &str,
        author_uri: &str,
        group_uri: &str,
        subject: &str,
        body: &str,
        published_unix: i64,
    ) -> BoardItem {
        BoardItem {
            context: AS_CONTEXT,
            kind: "Page",
            id: ap_id.to_string(),
            attributed_to: author_uri.to_string(),
            name: subject.to_string(),
            content: format!("<p>{}</p>", html_escape(body)),
            published: crate::util::fmt_rfc3339(published_unix),
            to: vec![group_uri.to_string()],
            cc: vec![PUBLIC.to_string()],
            audience: group_uri.to_string(),
            in_reply_to: None,
        }
    }

    /// Wrap a submission in the author's `Create`. Unlike [`board_announce`],
    /// the **author** signs and owns this — we're a contributor to that board,
    /// not its hub.
    pub fn remote_board_create(page: BoardItem) -> CreatePage {
        CreatePage {
            context: AS_CONTEXT,
            kind: "Create",
            id: format!("{}/activity", page.id),
            actor: page.attributed_to.clone(),
            to: page.to.clone(),
            cc: page.cc.clone(),
            audience: page.audience.clone(),
            object: page,
        }
    }

    /// A `Delete` as a board relays it: the **author** is the actor (they're the
    /// one withdrawing), and the object is the post's permanent URI.
    #[derive(Debug, Clone, Serialize)]
    pub struct DeletePage {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub object: String,
    }

    /// The Group's `Announce{Delete}` (#133).
    #[derive(Debug, Clone, Serialize)]
    pub struct AnnounceDelete {
        #[serde(rename = "@context")]
        pub context: &'static str,
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub id: String,
        pub actor: String,
        pub to: Vec<String>,
        pub cc: Vec<String>,
        pub object: DeletePage,
    }

    /// Build the Group's `Announce{Delete}` for a withdrawn board post, so
    /// subscribers drop it from their mirrors (#133).
    ///
    /// The inner `Delete`'s actor is the **author**, not the Group — that's what
    /// lets a receiver authorize the withdrawal against the post's attribution
    /// rather than taking the relaying board's word for it.
    pub fn board_delete(
        origin: &Origin,
        slug: &str,
        author_uri: &str,
        local_id: i64,
        post_ap_id: &str,
    ) -> AnnounceDelete {
        let group_uri = origin.group(slug);
        let followers = origin.group_followers(slug);
        let to = vec![PUBLIC.to_string()];
        let cc = vec![followers];
        let delete = DeletePage {
            context: AS_CONTEXT,
            kind: "Delete",
            id: format!("{post_ap_id}/delete"),
            actor: author_uri.to_string(),
            to: to.clone(),
            cc: cc.clone(),
            object: post_ap_id.to_string(),
        };
        AnnounceDelete {
            context: AS_CONTEXT,
            kind: "Announce",
            // From our origin, not the post's — see [`board_announce`] for why
            // deriving an activity id from the object breaks on remote content.
            id: format!("{group_uri}/announce/delete/{local_id}"),
            actor: group_uri,
            to,
            cc,
            object: delete,
        }
    }

    /// Wrap a board `Page` in the Group's `Announce{Create{Page}}` — the object a
    /// subscriber receives (and the outbox item). The Group signs and owns the
    /// Announce; the inner Create keeps the author's attribution intact.
    /// `local_id` is the post's row id in `messages`, and it is what the
    /// `Announce`'s own id is minted from — **not** the post's URI.
    ///
    /// That distinction matters as soon as the post came from somewhere else.
    /// An activity's id must belong to the instance that created it, and *we*
    /// create this Announce; deriving it from the post would put our activity
    /// under the author's domain. When the announcement then reaches that
    /// author's instance, it sees an id on its own domain and rejects the whole
    /// thing as spoofed ("Activity was sent from local instance") — so the
    /// author's own server is the one instance that can never receive it.
    pub fn board_announce(
        origin: &Origin,
        slug: &str,
        author_uri: &str,
        local_id: i64,
        page: BoardItem,
    ) -> Announce {
        let group_uri = origin.group(slug);
        let followers = origin.group_followers(slug);
        let create = CreatePage {
            context: AS_CONTEXT,
            kind: "Create",
            id: format!("{}/activity", page.id),
            actor: author_uri.to_string(),
            to: page.to.clone(),
            cc: page.cc.clone(),
            audience: group_uri.clone(),
            object: page,
        };
        Announce {
            context: AS_CONTEXT,
            kind: "Announce",
            id: format!("{group_uri}/announce/{local_id}"),
            actor: group_uri,
            to: vec![PUBLIC.to_string()],
            cc: vec![followers],
            object: create,
        }
    }
}

/// Build the AP object for a board message — the single place that decides what
/// a board post looks like on the wire (#139).
///
/// Extracted because there were two hand-written copies of this (the fan-out and
/// the Group outbox) and they had already drifted apart in two ways: the outbox
/// never learned about replies, and it minted `attributedTo` from our own origin
/// for *every* author, so a post by `bob@remote.social` was published as
/// `https://ours/u/bob@remote.social` — claiming someone else's content as ours.
/// One builder, one answer.
///
/// Returns the item plus its `ap_id`, which is minted here if the message
/// doesn't have one yet.
pub async fn board_item_for(
    pool: &SqlitePool,
    origin: &Origin,
    slug: &str,
    msg: &crate::db::models::Message,
) -> Result<(objects::BoardItem, String)> {
    let ap_id = ensure_message_ap_id(pool, origin, msg.id).await?;
    // A remote author keeps their own actor URI; a local author's is minted from
    // our origin. Getting this backwards is how you accidentally claim to have
    // authored a peer's post.
    let author_uri: String =
        sqlx::query_scalar("SELECT actor_uri FROM users WHERE id = ? AND is_remote = 1")
            .bind(msg.author_id)
            .fetch_optional(pool)
            .await?
            .flatten()
            .unwrap_or_else(|| origin.person(&msg.author_name));

    let item = match msg.parent_id {
        None => objects::board_page(
            origin,
            slug,
            &ap_id,
            &author_uri,
            &msg.subject,
            &msg.body,
            msg.created_at,
        ),
        Some(parent_id) => {
            // The parent's URI, whoever owns it. `ensure_message_ap_id` returns
            // a stored one unchanged, so a parent that arrived from a peer keeps
            // *its* id and the thread stays joined up across instances rather
            // than forking at our boundary.
            let parent_uri = ensure_message_ap_id(pool, origin, parent_id).await?;
            objects::board_reply(
                origin,
                slug,
                &ap_id,
                &author_uri,
                &msg.subject,
                &msg.body,
                msg.created_at,
                &parent_uri,
            )
        }
    };
    Ok((item, author_uri))
}

/// Record a status's permanent `ap_id`, if it doesn't have one yet.
///
/// Lazy, like actor keys: a board that never federates mints no URIs. The value
/// is derivable from the local id, but it's *stored* so it stays fixed even if
/// the configured origin later changes — the URI is already out in the world by
/// then.
pub async fn ensure_status_ap_id(pool: &SqlitePool, origin: &Origin, id: i64) -> Result<String> {
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT ap_id FROM oneliners WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    match existing {
        None => Err(AppError::NotFound),
        Some(Some(ap_id)) => Ok(ap_id),
        Some(None) => {
            let ap_id = origin.status(id);
            sqlx::query("UPDATE oneliners SET ap_id = ? WHERE id = ?")
                .bind(&ap_id)
                .bind(id)
                .execute(pool)
                .await?;
            Ok(ap_id)
        }
    }
}

/// Look up a local user by the username in an `acct:` URI, for WebFinger.
///
/// Only local, federatable accounts resolve: `guest` is shared (one keypair for
/// many humans means one abuse report suspends everyone), and remote rows
/// belong to other servers.
pub async fn find_local_actor(pool: &SqlitePool, username: &str) -> Result<Option<User>> {
    let user = crate::services::auth::find_user(pool, username).await?;
    Ok(user.filter(|u| !u.is_remote && !u.is_guest()))
}

/// The durable outbound delivery queue.
///
/// The AP crate ships its own queue, but it's **in-memory** and retries at
/// roughly 1min / 1hr / 2.5 days — a restart inside that window silently drops
/// deliveries, which is a known Lemmy pain point. Since we already have SQLite,
/// we persist instead and accept at-least-once semantics.
///
/// This is the storage half. The drain — signing each activity with its actor's
/// key and POSTing it — lands in #109, which is where the first real delivery
/// target appears: nothing can be delivered until an inbox accepts a `Follow`
/// and gives us a follower.
pub mod queue {
    use super::*;
    use crate::util::now_unix;

    /// A delivery waiting to go out.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Delivery {
        pub id: i64,
        /// Actor whose key signs the request.
        pub actor_uri: String,
        pub inbox_url: String,
        /// The serialized activity JSON.
        pub activity: String,
        pub attempts: i64,
    }

    /// Queue an activity for delivery to one inbox.
    ///
    /// One row per (activity, inbox): a `Create` going to 50 followers is 50
    /// rows, so one dead server can't stall the other 49.
    pub async fn enqueue(
        pool: &SqlitePool,
        actor_uri: &str,
        inbox_url: &str,
        activity: &str,
        activity_uri: Option<&str>,
    ) -> Result<i64> {
        let id = sqlx::query(
            "INSERT INTO ap_deliveries \
             (actor_uri, inbox_url, activity, activity_uri, next_attempt_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(actor_uri)
        .bind(inbox_url)
        .bind(activity)
        .bind(activity_uri)
        .bind(now_unix())
        .bind(now_unix())
        .execute(pool)
        .await?
        .last_insert_rowid();
        Ok(id)
    }

    /// Deliveries whose backoff has elapsed, oldest first.
    pub async fn due(pool: &SqlitePool, limit: i64) -> Result<Vec<Delivery>> {
        let rows = sqlx::query_as::<_, Delivery>(
            "SELECT id, actor_uri, inbox_url, activity, attempts FROM ap_deliveries \
             WHERE next_attempt_at <= ? ORDER BY id LIMIT ?",
        )
        .bind(now_unix())
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Drop a delivered activity. The queue holds only outstanding work — the
    /// audit trail of what we sent is the activity's own object.
    pub async fn mark_delivered(pool: &SqlitePool, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM ap_deliveries WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Exponential backoff for attempt `n` (1-based), capped at ~2.7 hours.
    ///
    /// Remote outages are routine on the fediverse, so retries are patient
    /// rather than aggressive: 1m, 2m, 4m … A caller that hammers a struggling
    /// server is how you get defederated.
    pub fn backoff_secs(attempts: i64) -> i64 {
        const BASE: i64 = 60;
        const CAP: i64 = 60 * 60 * 3;
        BASE.saturating_mul(
            1i64.checked_shl(attempts.clamp(0, 16) as u32)
                .unwrap_or(i64::MAX),
        )
        .min(CAP)
    }

    /// Record a failed attempt: back off, or give up past `max_attempts`.
    ///
    /// Returns `true` if the delivery was dropped for good. Giving up is normal
    /// — a peer can vanish permanently, and a queue that never forgets grows
    /// without bound.
    pub async fn mark_failed(
        pool: &SqlitePool,
        id: i64,
        error: &str,
        max_attempts: u32,
    ) -> Result<bool> {
        let attempts: Option<i64> =
            sqlx::query_scalar("SELECT attempts FROM ap_deliveries WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        let Some(attempts) = attempts else {
            return Ok(false);
        };
        let attempts = attempts + 1;
        if attempts >= max_attempts as i64 {
            tracing::warn!("giving up on delivery {id} after {attempts} attempts: {error}");
            mark_delivered(pool, id).await?;
            return Ok(true);
        }
        sqlx::query(
            "UPDATE ap_deliveries SET attempts = ?, next_attempt_at = ?, last_error = ? \
             WHERE id = ?",
        )
        .bind(attempts)
        .bind(now_unix() + backoff_secs(attempts))
        .bind(error)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(false)
    }

    /// How many deliveries are outstanding (operator visibility).
    pub async fn pending(pool: &SqlitePool) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM ap_deliveries")
            .fetch_one(pool)
            .await?)
    }
}

/// Domain-level federation policy, over the `ap_blocks` table.
///
/// Two postures (`[federation] allowlist_only`):
/// - **allowlist** (default): only domains with an `allow` row may federate.
///   For a small board this is a feature, not a limitation — open federation
///   means volunteering to moderate the entire internet.
/// - **blocklist**: anyone may federate except domains with a `block` row.
pub mod policy {
    use super::*;
    use crate::util::now_unix;

    /// Whether `domain` may federate with us under the given posture. Our own
    /// origin host is always allowed (we federate with ourselves).
    pub async fn domain_allowed(
        pool: &SqlitePool,
        origin_host: &str,
        domain: &str,
        allowlist_only: bool,
    ) -> Result<bool> {
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Ok(false);
        }
        if domain.eq_ignore_ascii_case(origin_host) {
            return Ok(true);
        }
        // A *suspended* domain is refused in either posture — that's the hard
        // block. A `silence` block is not refused here: it may still federate,
        // and is filtered at content ingestion instead (see `domain_silenced`).
        let suspended: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_blocks \
             WHERE kind = 'block' AND severity = 'suspend' AND lower(domain) = ?",
        )
        .bind(&domain)
        .fetch_one(pool)
        .await?;
        if suspended > 0 {
            return Ok(false);
        }
        if !allowlist_only {
            return Ok(true);
        }
        let allowed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_blocks WHERE kind = 'allow' AND lower(domain) = ?",
        )
        .bind(&domain)
        .fetch_one(pool)
        .await?;
        Ok(allowed > 0)
    }

    /// Whether a domain is *silenced*: it may still federate, but nothing it
    /// sends is accepted into a shared surface (boards, timeline, mirrors).
    ///
    /// This is the middle setting between "fully trusted" and "defederated" —
    /// useful when a peer isn't malicious enough to cut off but shouldn't be
    /// filling your board.
    pub async fn domain_silenced(pool: &SqlitePool, domain: &str) -> Result<bool> {
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Ok(false);
        }
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_blocks \
             WHERE kind = 'block' AND severity = 'silence' AND lower(domain) = ?",
        )
        .bind(&domain)
        .fetch_one(pool)
        .await?;
        Ok(n > 0)
    }

    /// Whether the actor at `uri` belongs to a silenced domain.
    pub async fn actor_silenced(pool: &SqlitePool, actor_uri: &str) -> Result<bool> {
        let host = url::Url::parse(actor_uri)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        domain_silenced(pool, &host).await
    }

    /// Add or update a policy row (`kind` = "allow" | "block"). `severity` is
    /// `"suspend"` (hard block) or `"silence"`, and only applies to blocks.
    pub async fn set(
        pool: &SqlitePool,
        domain: &str,
        kind: &str,
        reason: &str,
        severity: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO ap_blocks (domain, kind, reason, created_at, severity) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(domain, kind) DO UPDATE SET \
               reason = excluded.reason, severity = excluded.severity",
        )
        .bind(domain.trim().to_ascii_lowercase())
        .bind(kind)
        .bind(reason)
        .bind(now_unix())
        .bind(severity)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Remove a policy row. Returns whether one existed.
    pub async fn unset(pool: &SqlitePool, domain: &str, kind: &str) -> Result<bool> {
        let n = sqlx::query("DELETE FROM ap_blocks WHERE lower(domain) = ? AND kind = ?")
            .bind(domain.trim().to_ascii_lowercase())
            .bind(kind)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// All policy rows of a kind, for operator display.
    pub async fn list(pool: &SqlitePool, kind: &str) -> Result<Vec<(String, String, String)>> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT domain, reason, severity FROM ap_blocks WHERE kind = ? ORDER BY domain",
        )
        .bind(kind)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}

/// The follower graph, over the `ap_follows` table.
///
/// One row = one directed edge: `actor_uri` follows `object_uri`. For an
/// *inbound* follow (a remote actor following one of our users) the local side
/// is `object_uri`; for an *outbound* follow (a local user following a remote
/// account, added in Slice C) the local side is `actor_uri`.
pub mod follows {
    use super::*;
    use crate::util::now_unix;

    /// Whether a *local* user follows `author_uri` with an accepted follow.
    ///
    /// This is the gate on inbound statuses: we cache a remote `Note` only if
    /// someone here actually asked to see that author's posts. Since `author_uri`
    /// is remote, only outbound edges (local → remote) can match, and the join
    /// confirms the follower is a real local account.
    pub async fn is_followed_locally(pool: &SqlitePool, author_uri: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_follows f JOIN users u ON u.actor_uri = f.actor_uri \
             WHERE f.object_uri = ? AND f.state = 'accepted' AND u.is_remote = 0",
        )
        .bind(author_uri)
        .fetch_one(pool)
        .await?;
        Ok(n > 0)
    }

    /// Record a remote actor's accepted follow of a local actor. Idempotent on
    /// `(follower, followed)`; refreshes the stored `Follow` id on a repeat.
    /// Returns the row id, which seeds a stable `Accept` activity id.
    pub async fn accept(
        pool: &SqlitePool,
        follower_uri: &str,
        followed_uri: &str,
        follow_uri: &str,
    ) -> Result<i64> {
        sqlx::query(
            "INSERT INTO ap_follows (actor_uri, object_uri, state, follow_uri, created_at) \
             VALUES (?, ?, 'accepted', ?, ?) \
             ON CONFLICT(actor_uri, object_uri) DO UPDATE SET \
               state = 'accepted', follow_uri = excluded.follow_uri",
        )
        .bind(follower_uri)
        .bind(followed_uri)
        .bind(follow_uri)
        .bind(now_unix())
        .execute(pool)
        .await?;
        // last_insert_rowid isn't reliable across an upsert that took the UPDATE
        // path, so read the id back by its unique key.
        let id: i64 =
            sqlx::query_scalar("SELECT id FROM ap_follows WHERE actor_uri = ? AND object_uri = ?")
                .bind(follower_uri)
                .bind(followed_uri)
                .fetch_one(pool)
                .await?;
        Ok(id)
    }

    /// Record an *outbound* follow request (a local user following a remote
    /// account) as `pending`, before we've heard back. Idempotent: a repeat
    /// refreshes the `Follow` id but never downgrades an already-`accepted` edge.
    pub async fn request(
        pool: &SqlitePool,
        follower_uri: &str,
        followed_uri: &str,
        follow_uri: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO ap_follows (actor_uri, object_uri, state, follow_uri, created_at) \
             VALUES (?, ?, 'pending', ?, ?) \
             ON CONFLICT(actor_uri, object_uri) DO UPDATE SET follow_uri = excluded.follow_uri",
        )
        .bind(follower_uri)
        .bind(followed_uri)
        .bind(follow_uri)
        .bind(now_unix())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Mark an outbound follow `accepted` — a remote server answered our
    /// `Follow` with an `Accept`. Returns whether a matching pending edge existed.
    pub async fn mark_accepted(
        pool: &SqlitePool,
        follower_uri: &str,
        followed_uri: &str,
    ) -> Result<bool> {
        let n = sqlx::query(
            "UPDATE ap_follows SET state = 'accepted' WHERE actor_uri = ? AND object_uri = ?",
        )
        .bind(follower_uri)
        .bind(followed_uri)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// The remote accounts a local actor follows, for display. Each row is
    /// `(followed actor URI, state)`.
    pub async fn following(pool: &SqlitePool, follower_uri: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT object_uri, state FROM ap_follows WHERE actor_uri = ? ORDER BY object_uri",
        )
        .bind(follower_uri)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// Drop a follow (an inbound `Undo{Follow}`, or a local unfollow). Returns
    /// whether a row existed.
    pub async fn remove(pool: &SqlitePool, follower_uri: &str, followed_uri: &str) -> Result<bool> {
        let n = sqlx::query("DELETE FROM ap_follows WHERE actor_uri = ? AND object_uri = ?")
            .bind(follower_uri)
            .bind(followed_uri)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Distinct inboxes to deliver a local actor's status to: one per remote
    /// follower, collapsed onto shared inboxes where a server offers one.
    pub async fn follower_inboxes(pool: &SqlitePool, followed_uri: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT COALESCE(u.shared_inbox_url, u.inbox_url) AS inbox \
             FROM ap_follows f JOIN users u ON u.actor_uri = f.actor_uri \
             WHERE f.object_uri = ? AND f.state = 'accepted' AND u.inbox_url IS NOT NULL",
        )
        .bind(followed_uri)
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(|(i,)| i).collect())
    }

    /// How many remote followers a local actor has (operator visibility / tests).
    pub async fn count(pool: &SqlitePool, followed_uri: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM ap_follows WHERE object_uri = ? AND state = 'accepted'",
        )
        .bind(followed_uri)
        .fetch_one(pool)
        .await?)
    }
}

/// Outbound delivery: turning a local event into queued activities.
pub mod outbound {
    use super::*;

    /// Fan a freshly-posted status out to its author's remote followers.
    ///
    /// Builds the `Create{Note}` once and enqueues one delivery per distinct
    /// follower inbox (see [`queue::enqueue`] for why one row per inbox). Returns
    /// the number of deliveries queued — `0` when the author has no remote
    /// followers, which is the common case and not an error.
    ///
    /// This only *enqueues*; the [`queue`] drain signs and POSTs. It needs no
    /// federation library handle, just the pool and a validated origin, so it's
    /// safe to call straight from the post path.
    pub async fn deliver_status(
        pool: &SqlitePool,
        origin: &Origin,
        oneliner_id: i64,
    ) -> Result<usize> {
        let Some(o) = crate::services::oneliners::get(pool, oneliner_id).await? else {
            return Ok(0);
        };
        // Only local, federatable authors have followers to deliver to.
        let Some(author) = find_local_actor(pool, &o.author_name).await? else {
            return Ok(0);
        };
        let actor_uri = origin.person(&author.username);
        let inboxes = follows::follower_inboxes(pool, &actor_uri).await?;
        if inboxes.is_empty() {
            return Ok(0);
        }

        let create = objects::create_for(pool, origin, &o).await?;
        let activity = serde_json::to_string(&create)
            .map_err(|e| AppError::Other(anyhow::anyhow!("serializing Create{{Note}}: {e}")))?;

        let mut queued = 0;
        for inbox in inboxes {
            queue::enqueue(pool, &actor_uri, &inbox, &activity, Some(&create.id)).await?;
            queued += 1;
        }
        Ok(queued)
    }

    /// Fan a board post out to the board's Group followers as the Group's
    /// `Announce{Create{…}}` (FEP-1b12, #111) — a `Page` for a root, a `Note`
    /// with `inReplyTo` for a reply (#139).
    ///
    /// Signed by the **Group**, not the author, when the drain picks it up.
    /// Returns the number of deliveries queued — `0` when the board has no
    /// remote subscribers (the common case) or federation can't build the Group.
    ///
    /// This is also the function the *inbound* path calls to re-Announce a post
    /// that arrived at one of our boards, which is why replies being skipped
    /// here was a correctness bug and not just a missing feature: we are that
    /// board's hub, so a reply we accepted but never relayed left every other
    /// subscriber with a permanently divergent thread.
    pub async fn deliver_board_post(
        pool: &SqlitePool,
        origin: &Origin,
        message_id: i64,
    ) -> Result<usize> {
        let msg = crate::services::boards::get_message(pool, message_id).await?;
        let keys = ensure_group_keys(pool, origin, msg.board_id).await?;
        let inboxes = follows::follower_inboxes(pool, &keys.actor_uri).await?;
        if inboxes.is_empty() {
            return Ok(0);
        }

        let (item, author_uri) = board_item_for(pool, origin, &keys.slug, &msg).await?;
        let announce = objects::board_announce(origin, &keys.slug, &author_uri, msg.id, item);
        let activity = serde_json::to_string(&announce)
            .map_err(|e| AppError::Other(anyhow::anyhow!("serializing Announce: {e}")))?;

        let mut queued = 0;
        for inbox in inboxes {
            queue::enqueue(pool, &keys.actor_uri, &inbox, &activity, Some(&announce.id)).await?;
            queued += 1;
        }
        Ok(queued)
    }

    /// A built, addressed activity waiting to be queued.
    ///
    /// Exists because announcing a *deletion* has an ordering problem the other
    /// fan-outs don't: everything the activity needs (the post's `ap_id`, its
    /// board, its author) lives in the row that's about to disappear. So the
    /// activity is built first and queued after the delete succeeds — never the
    /// reverse, which would tell subscribers to drop a post we then failed to
    /// remove ourselves.
    pub struct Prepared {
        actor_uri: String,
        activity: String,
        activity_id: String,
        inboxes: Vec<String>,
    }

    /// Build the Group's `Announce{Delete}` for a board post *before* it is
    /// deleted (#133). `None` when there's nothing to announce: a post that
    /// never federated (no `ap_id`), or a board with no remote subscribers.
    ///
    /// Replies are included since #139 — they syndicate now, so a withdrawn one
    /// has to be withdrawn everywhere. The `ap_id` check below is what makes
    /// that safe either way: a reply from before replies syndicated never got a
    /// URI, so there's nothing to withdraw and this still returns `None`.
    pub async fn prepare_board_delete(
        pool: &SqlitePool,
        origin: &Origin,
        message_id: i64,
    ) -> Result<Option<Prepared>> {
        let msg = crate::services::boards::get_message(pool, message_id).await?;
        // Deliberately *not* `ensure_message_ap_id`: a post that never had a URI
        // was never syndicated, so there is nothing for anyone to withdraw.
        let Some(ap_id): Option<String> =
            sqlx::query_scalar("SELECT ap_id FROM messages WHERE id = ?")
                .bind(message_id)
                .fetch_optional(pool)
                .await?
                .flatten()
        else {
            return Ok(None);
        };

        let keys = ensure_group_keys(pool, origin, msg.board_id).await?;
        let inboxes = follows::follower_inboxes(pool, &keys.actor_uri).await?;
        if inboxes.is_empty() {
            return Ok(None);
        }

        let author_uri: String =
            sqlx::query_scalar("SELECT actor_uri FROM users WHERE id = ? AND is_remote = 1")
                .bind(msg.author_id)
                .fetch_optional(pool)
                .await?
                .flatten()
                .unwrap_or_else(|| origin.person(&msg.author_name));
        let announce = objects::board_delete(origin, &keys.slug, &author_uri, message_id, &ap_id);
        let activity = serde_json::to_string(&announce)
            .map_err(|e| AppError::Other(anyhow::anyhow!("serializing Announce{{Delete}}: {e}")))?;
        Ok(Some(Prepared {
            actor_uri: keys.actor_uri,
            activity,
            activity_id: announce.id,
            inboxes,
        }))
    }

    /// Queue a [`Prepared`] activity to every addressed inbox.
    pub async fn dispatch(pool: &SqlitePool, p: &Prepared) -> Result<usize> {
        for inbox in &p.inboxes {
            queue::enqueue(pool, &p.actor_uri, inbox, &p.activity, Some(&p.activity_id)).await?;
        }
        Ok(p.inboxes.len())
    }
}

/// Content degradation: fediverse content is HTML, a terminal is not.
///
/// Remote statuses (and, later, board posts) arrive as HTML with links, inline
/// images, and custom emoji. A BBS renders text, so we flatten to plain text at
/// ingestion — the timeline stores what we can actually show. This is a small
/// hand-rolled pass, not a full HTML engine: it handles the tags Mastodon
/// actually emits (`p`, `br`, `a`, `img`) and strips the rest, which is all
/// status markup uses. Anything fancier would be a dependency we don't need.
pub mod content {
    /// Flatten a fragment of status HTML to plain text.
    ///
    /// - `<p>` and `<br>` become line breaks; block structure collapses to lines.
    /// - `<a href="U">text</a>` keeps its text, appending ` (U)` when the URL
    ///   isn't already the visible text (Mastodon often shows a truncated URL).
    /// - `<img alt="A" src="U">` becomes `[img: A] (U)` — the image can't render,
    ///   but its description and source survive.
    /// - all other tags are dropped; HTML entities are decoded.
    pub fn html_to_text(html: &str) -> String {
        let mut out = String::with_capacity(html.len());
        let mut i = 0;
        // An open `<a>`: its href, and where its visible text began in `out`, so
        // the closing tag can decide whether the URL adds anything.
        let mut open_link: Option<(String, usize)> = None;
        while i < html.len() {
            if html.as_bytes()[i] == b'<' {
                let Some(close) = html[i..].find('>').map(|o| i + o) else {
                    // Unclosed '<' — the rest is text.
                    decode_entities_into(&html[i..], &mut out);
                    break;
                };
                let tag = &html[i + 1..close];
                let closing = tag.starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split([' ', '\t', '\n', '/'])
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    "br" => out.push('\n'),
                    "p" | "div" | "blockquote" | "li" | "ul" | "ol" => {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    "a" if !closing => {
                        open_link = Some((attr(tag, "href").unwrap_or_default(), out.len()))
                    }
                    "a" if closing => {
                        if let Some((href, text_start)) = open_link.take() {
                            let text = out[text_start..].trim().to_string();
                            if href.is_empty() {
                                // nothing to add
                            } else if text.is_empty() {
                                out.push_str(&href);
                            } else if !same_link(&text, &href) {
                                out.push_str(&format!(" ({href})"));
                            }
                        }
                    }
                    "img" => {
                        let alt = attr(tag, "alt").unwrap_or_default();
                        let src = attr(tag, "src").unwrap_or_default();
                        if alt.is_empty() {
                            out.push_str("[img]");
                        } else {
                            out.push_str(&format!("[img: {alt}]"));
                        }
                        if !src.is_empty() {
                            out.push_str(&format!(" ({src})"));
                        }
                    }
                    _ => {}
                }
                i = close + 1;
            } else {
                let next = html[i..].find('<').map(|o| i + o).unwrap_or(html.len());
                decode_entities_into(&html[i..next], &mut out);
                i = next;
            }
        }
        normalize(&out)
    }

    /// Whether a link's visible text already conveys its href, so appending the
    /// URL would just duplicate it. Compares with scheme and a trailing slash
    /// stripped: `example.com/` matches `https://example.com`, but a *truncated*
    /// label (`example.com` for `.../page`) does not, so the full URL is kept.
    fn same_link(text: &str, href: &str) -> bool {
        let norm = |s: &str| {
            s.trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string()
        };
        norm(text) == norm(href)
    }

    /// Extract an attribute value (`name="..."` or `name='...'`) from a tag body.
    fn attr(tag: &str, name: &str) -> Option<String> {
        let lower = tag.to_ascii_lowercase();
        let key = format!("{name}=");
        let at = lower.find(&key)? + key.len();
        let rest = &tag[at..];
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let end = rest[1..].find(quote)? + 1;
            Some(decode_entities(&rest[1..end]))
        } else {
            let end = rest.find([' ', '\t']).unwrap_or(rest.len());
            Some(decode_entities(&rest[..end]))
        }
    }

    fn decode_entities(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        decode_entities_into(s, &mut out);
        out
    }

    fn decode_entities_into(s: &str, out: &mut String) {
        let mut rest = s;
        while let Some(amp) = rest.find('&') {
            out.push_str(&rest[..amp]);
            let after = &rest[amp..];
            let Some(semi) = after.find(';').filter(|&p| p <= 8) else {
                out.push('&');
                rest = &after[1..];
                continue;
            };
            let entity = &after[1..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            match decoded {
                Some(c) => out.push(c),
                None => out.push_str(&after[..=semi]), // keep unknown entity verbatim
            }
            rest = &after[semi + 1..];
        }
        out.push_str(rest);
    }

    /// Trim per-line trailing space and squeeze runs of blank lines to one.
    fn normalize(s: &str) -> String {
        let lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
        let mut result = String::new();
        let mut prev_blank = true; // suppresses leading blanks
        for line in lines {
            let blank = line.is_empty();
            if blank && prev_blank {
                continue;
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
            prev_blank = blank;
        }
        // Drop a single trailing blank line if present.
        while result.ends_with('\n') {
            result.pop();
        }
        result
    }
}

/// The inbound timeline: cached remote statuses (`ap_timeline`), the read model
/// behind the timeline screen (#109 Slice C).
pub mod timeline {
    use super::*;
    use crate::util::now_unix;

    /// A cached remote status, shaped for display.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Entry {
        pub id: i64,
        pub ap_id: String,
        pub author_uri: String,
        pub author_handle: String,
        pub content: String,
        pub url: Option<String>,
        pub published: i64,
    }

    /// Store a received status. Idempotent on the Note's `ap_id`: a redelivery
    /// (or a status that reaches several of our followers) inserts once. Returns
    /// whether a new row was created.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        pool: &SqlitePool,
        ap_id: &str,
        author_uri: &str,
        author_handle: &str,
        content: &str,
        url: Option<&str>,
        published: i64,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "INSERT INTO ap_timeline \
               (ap_id, author_uri, author_handle, content, url, published, received_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(ap_id) DO NOTHING",
        )
        .bind(ap_id)
        .bind(author_uri)
        .bind(author_handle)
        .bind(content)
        .bind(url)
        .bind(published)
        .bind(now_unix())
        .execute(pool)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Recent statuses, newest first.
    pub async fn recent(pool: &SqlitePool, limit: i64) -> Result<Vec<Entry>> {
        let rows = sqlx::query_as::<_, Entry>(
            "SELECT id, ap_id, author_uri, author_handle, content, url, published \
             FROM ap_timeline ORDER BY published DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// How many cached statuses there are (operator visibility / tests).
    pub async fn count(pool: &SqlitePool) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM ap_timeline")
            .fetch_one(pool)
            .await?)
    }
}

/// Mirrored posts from followed remote boards (`ap_board_posts`), the read model
/// behind a subscribed remote board (#111 Slice C).
pub mod mirror {
    use super::*;
    use crate::util::now_unix;

    /// A cached post from a remote board, shaped for display.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Post {
        pub id: i64,
        pub ap_id: String,
        pub group_uri: String,
        pub group_handle: String,
        pub author_handle: String,
        pub subject: String,
        pub content: String,
        pub url: Option<String>,
        pub published: i64,
        /// The parent's URI when this is a reply (#139). A URI rather than a
        /// local id because mirrored posts arrive out of order — a reply
        /// routinely lands before the post it answers.
        pub in_reply_to: Option<String>,
    }

    /// A mirrored post with its depth in the reply tree (0 = thread root),
    /// mirroring `boards::ThreadItem` for local boards.
    #[derive(Debug, Clone)]
    pub struct ThreadedPost {
        pub post: Post,
        pub depth: u16,
    }

    /// Store a mirrored board post. Idempotent on the Page's `ap_id`: a
    /// redelivery (or a post reaching us via two paths) inserts once. Returns
    /// whether a new row was created.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        pool: &SqlitePool,
        ap_id: &str,
        group_uri: &str,
        group_handle: &str,
        author_handle: &str,
        author_uri: &str,
        subject: &str,
        content: &str,
        url: Option<&str>,
        published: i64,
        in_reply_to: Option<&str>,
    ) -> Result<bool> {
        let affected = sqlx::query(
            "INSERT INTO ap_board_posts \
               (ap_id, group_uri, group_handle, author_handle, author_uri, subject, content, url, \
                published, received_at, in_reply_to) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(ap_id) DO NOTHING",
        )
        .bind(ap_id)
        .bind(group_uri)
        .bind(group_handle)
        .bind(author_handle)
        .bind(author_uri)
        .bind(subject)
        .bind(content)
        .bind(url)
        .bind(published)
        .bind(now_unix())
        .bind(in_reply_to)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Recent posts from a subscribed remote board, newest first.
    pub async fn recent(pool: &SqlitePool, group_uri: &str, limit: i64) -> Result<Vec<Post>> {
        let rows = sqlx::query_as::<_, Post>(
            "SELECT id, ap_id, group_uri, group_handle, author_handle, subject, content, url, \
             published, in_reply_to FROM ap_board_posts WHERE group_uri = ? \
             ORDER BY published DESC, id DESC LIMIT ?",
        )
        .bind(group_uri)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// A remote board we subscribe to, as the in-BBS screen lists it (#132).
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Board {
        /// `slug@host` — the handle a user would type.
        pub handle: String,
        pub group_uri: String,
        /// `pending` until the remote side answers our `Follow` with an
        /// `Accept`. A pending board is legitimately empty, which is why the
        /// screen shows this rather than letting it read as a bug.
        pub state: String,
        pub posts: i64,
        /// When the newest mirrored post was published; `None` while empty.
        pub latest: Option<i64>,
    }

    /// Remote boards any local user subscribes to, with mirror stats.
    ///
    /// A followed actor counts as a board when we recorded its type as `Group`
    /// (migration 0019) — or, for follows predating that column, when it has
    /// actually announced something. The second clause is what keeps boards
    /// followed before the upgrade from vanishing off their own screen.
    pub async fn boards(pool: &SqlitePool) -> Result<Vec<Board>> {
        // `MIN(f.state)`: several local users may follow the same board with
        // different states. Mirroring is instance-wide, so if any edge is
        // accepted the board is live for everyone — and 'accepted' sorts before
        // 'pending', so MIN picks it. Without an aggregate here SQLite would
        // take an arbitrary row's state, which is a coin flip in exactly the
        // case the screen is trying to explain.
        let rows = sqlx::query_as::<_, Board>(
            "SELECT u.username AS handle, \
                    f.object_uri AS group_uri, \
                    MIN(f.state) AS state, \
                    (SELECT COUNT(*) FROM ap_board_posts p WHERE p.group_uri = f.object_uri) \
                      AS posts, \
                    (SELECT MAX(published) FROM ap_board_posts p WHERE p.group_uri = f.object_uri) \
                      AS latest \
               FROM ap_follows f \
               JOIN users u ON u.actor_uri = f.object_uri AND u.is_remote = 1 \
              WHERE u.actor_kind = 'Group' \
                 OR EXISTS (SELECT 1 FROM ap_board_posts p WHERE p.group_uri = f.object_uri) \
              GROUP BY f.object_uri \
              ORDER BY latest IS NULL, latest DESC, handle",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// A mirrored board's posts arranged as reply threads (#139 Slice B).
    ///
    /// Deliberately the same shape as [`crate::services::boards::list_thread`]:
    /// roots newest-first, each followed depth-first by its replies oldest-first,
    /// and **a reply whose parent we don't have becomes a root** rather than
    /// disappearing.
    ///
    /// That last rule matters more here than it does locally. A mirror is a
    /// partial view by construction — we only hold what a board announced while
    /// we were subscribed — so a reply arriving before (or without) its parent
    /// is normal, not corruption. Hiding it would silently drop real content;
    /// showing it unattached is honest about what we know.
    pub async fn thread(
        pool: &SqlitePool,
        group_uri: &str,
        limit: i64,
    ) -> Result<Vec<ThreadedPost>> {
        use std::collections::{HashMap, HashSet};

        let all = recent(pool, group_uri, limit).await?;
        let known: HashSet<String> = all.iter().map(|p| p.ap_id.clone()).collect();

        let mut children: HashMap<Option<String>, Vec<Post>> = HashMap::new();
        for p in all {
            // An `inReplyTo` we don't hold is treated as no parent at all.
            let parent = p
                .in_reply_to
                .clone()
                .filter(|uri| known.contains(uri.as_str()));
            children.entry(parent).or_default().push(p);
        }

        let mut roots = children.remove(&None).unwrap_or_default();
        let mut stack: Vec<(Post, u16)> = roots.drain(..).rev().map(|p| (p, 0)).collect();
        let mut order = Vec::new();
        while let Some((p, depth)) = stack.pop() {
            let ap_id = p.ap_id.clone();
            order.push(ThreadedPost { post: p, depth });
            if let Some(mut kids) = children.remove(&Some(ap_id)) {
                kids.sort_by_key(|k| (k.published, k.id));
                for k in kids.into_iter().rev() {
                    stack.push((k, depth + 1));
                }
            }
        }
        Ok(order)
    }

    /// The subject of a mirrored post, by URI — used to build `Re: <parent>`
    /// for a remote reply that carried no subject of its own.
    pub async fn subject_of(pool: &SqlitePool, ap_id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT subject FROM ap_board_posts WHERE ap_id = ?")
                .bind(ap_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// How many mirrored posts a board has (operator visibility / tests).
    pub async fn count(pool: &SqlitePool, group_uri: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM ap_board_posts WHERE group_uri = ?")
                .bind(group_uri)
                .fetch_one(pool)
                .await?,
        )
    }
}

/// Publishing into a board we don't own (#131).
///
/// The receiving half of this has worked since #112a — a `Create{Page}` whose
/// `audience` names a board Group is filed on that board. This is the sending
/// half, and the asymmetry with our own boards is deliberate: when *we* host a
/// board we `Announce` from the Group, because we're the hub. Here we're a
/// contributor, so the **author** signs a plain `Create` and the remote board
/// decides whether to publish it.
///
/// Which is also why a submission doesn't go straight into the mirror. We are
/// not the authority for that board, so a post is "awaiting the board" until the
/// board announces it back — at which point it lands in `ap_board_posts` under
/// the same `ap_id` and stops being pending. Showing it as published the moment
/// we queued it would be asserting something only the remote board can say.
pub mod remote_posting {
    use super::*;
    use crate::db::models::User;
    use crate::services::enforce_rate;
    use crate::util::now_unix;

    /// A post we've published into a remote board that the board hasn't
    /// announced back to us yet.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Pending {
        pub id: i64,
        pub ap_id: String,
        pub group_uri: String,
        pub author_handle: String,
        pub subject: String,
        pub body: String,
        pub created_at: i64,
    }

    /// Submit a post to a remote board: record it, mint its permanent URI, and
    /// queue the signed `Create{Page}` to the board's inbox.
    ///
    /// Returns the minted `ap_id`. Errors rather than silently no-oping when the
    /// board is unknown or the subscription isn't accepted — unlike a fan-out,
    /// this is a direct user action and deserves to be told it failed.
    pub async fn submit(
        pool: &SqlitePool,
        origin: &Origin,
        author: &User,
        group_uri: &str,
        subject: &str,
        body: &str,
        limits: &crate::config::Limits,
    ) -> Result<String> {
        if author.is_guest() {
            return Err(AppError::GuestNotAllowed);
        }
        // Same per-window budget as posting locally: federating shouldn't be a
        // way around the rate limit.
        let since = now_unix() - 3600;
        let count =
            crate::services::boards::author_post_count_since(pool, author.id, since).await?;
        enforce_rate(count, limits.max_posts)?;

        // The board must be one we actually subscribe to *and* that accepted us
        // — posting into a board that hasn't accepted our follow would be
        // shouting into a void the board is entitled to ignore.
        let state: Option<String> = sqlx::query_scalar(
            "SELECT MIN(state) FROM ap_follows WHERE object_uri = ? GROUP BY object_uri",
        )
        .bind(group_uri)
        .fetch_optional(pool)
        .await?;
        match state.as_deref() {
            Some("accepted") => {}
            Some(other) => {
                return Err(AppError::Other(anyhow::anyhow!(
                    "that board hasn't accepted the subscription yet ({other})"
                )));
            }
            None => {
                return Err(AppError::Other(anyhow::anyhow!(
                    "not subscribed to that board"
                )));
            }
        }
        let inbox: String =
            sqlx::query_scalar("SELECT inbox_url FROM users WHERE actor_uri = ? AND is_remote = 1")
                .bind(group_uri)
                .fetch_optional(pool)
                .await?
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("no inbox known for that board"))?;

        let created_at = now_unix();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO ap_outbox_posts (group_uri, author_id, subject, body, created_at) \
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(group_uri)
        .bind(author.id)
        .bind(subject)
        .bind(body)
        .bind(created_at)
        .fetch_one(pool)
        .await?;

        // The URI is minted from the row id, so it's stable from here on — an
        // AP object's id is a permanent primary key and can never be rewritten.
        let ap_id = origin.post(id);
        sqlx::query("UPDATE ap_outbox_posts SET ap_id = ? WHERE id = ?")
            .bind(&ap_id)
            .bind(id)
            .execute(pool)
            .await?;

        let author_uri = origin.person(&author.username);
        let page =
            objects::remote_board_page(&ap_id, &author_uri, group_uri, subject, body, created_at);
        let create = objects::remote_board_create(page);
        let activity = serde_json::to_string(&create)
            .map_err(|e| AppError::Other(anyhow::anyhow!("serializing Create{{Page}}: {e}")))?;
        queue::enqueue(pool, &author_uri, &inbox, &activity, Some(&create.id)).await?;
        Ok(ap_id)
    }

    /// Posts we've submitted to `group_uri` that the board hasn't announced back.
    ///
    /// The anti-join on `ap_board_posts` is what makes a pending post disappear
    /// on its own: once the board publishes it, the mirror copy is the real one
    /// and this stops returning it. No status column to keep in sync.
    pub async fn pending(pool: &SqlitePool, group_uri: &str) -> Result<Vec<Pending>> {
        let rows = sqlx::query_as::<_, Pending>(
            "SELECT o.id, o.ap_id, o.group_uri, u.username AS author_handle, \
                    o.subject, o.body, o.created_at \
               FROM ap_outbox_posts o \
               JOIN users u ON u.id = o.author_id \
              WHERE o.group_uri = ? \
                AND o.ap_id IS NOT NULL \
                AND NOT EXISTS (SELECT 1 FROM ap_board_posts p WHERE p.ap_id = o.ap_id) \
              ORDER BY o.id DESC",
        )
        .bind(group_uri)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}

/// Honoring a remote author's `Delete` / `Update` of content we accepted (#112).
///
/// Federated content lives in four stores, each keyed by the object's AP id:
/// board posts (`messages`), cached statuses (`ap_timeline`), mirrored board
/// posts (`ap_board_posts`), and inbound DMs (`mail`). Every operation here is
/// **authorized in the SQL itself** — the `WHERE` clause requires the acting
/// actor to own the row — so an actor can never touch another's content, and a
/// miss is indistinguishable from "not ours to change".
///
/// Deleting from `messages` also keeps the FTS index correct: the 0012 triggers
/// fire on the delete, so removed remote posts drop out of search too.
pub mod lifecycle {
    use super::*;

    /// Which store an operation landed in, for logging.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Target {
        BoardPost,
        Status,
        MirroredPost,
        Mail,
    }

    /// Delete a federated object on its owner's instruction. Returns where it
    /// landed, or `None` when we don't have it (or the actor doesn't own it).
    pub async fn delete(
        pool: &SqlitePool,
        actor_uri: &str,
        object_id: &str,
    ) -> Result<Option<Target>> {
        let n = sqlx::query(
            "DELETE FROM messages WHERE ap_id = ? \
             AND author_id IN (SELECT id FROM users WHERE actor_uri = ?)",
        )
        .bind(object_id)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::BoardPost));
        }

        let n = sqlx::query("DELETE FROM ap_timeline WHERE ap_id = ? AND author_uri = ?")
            .bind(object_id)
            .bind(actor_uri)
            .execute(pool)
            .await?
            .rows_affected();
        if n > 0 {
            return Ok(Some(Target::Status));
        }

        // A mirrored post may be withdrawn by its author *or* by the board that
        // announced it — in FEP-1b12 the Group is the authority for its content.
        let n = sqlx::query(
            "DELETE FROM ap_board_posts WHERE ap_id = ? AND (author_uri = ? OR group_uri = ?)",
        )
        .bind(object_id)
        .bind(actor_uri)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::MirroredPost));
        }

        let n = sqlx::query(
            "DELETE FROM mail WHERE ap_id = ? \
             AND from_id IN (SELECT id FROM users WHERE actor_uri = ?)",
        )
        .bind(object_id)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::Mail));
        }
        Ok(None)
    }

    /// Apply a remote edit. `subject` is ignored by stores that have none.
    pub async fn update(
        pool: &SqlitePool,
        actor_uri: &str,
        object_id: &str,
        subject: &str,
        content: &str,
    ) -> Result<Option<Target>> {
        let n = sqlx::query(
            "UPDATE messages SET subject = ?, body = ? WHERE ap_id = ? \
             AND author_id IN (SELECT id FROM users WHERE actor_uri = ?)",
        )
        .bind(subject)
        .bind(content)
        .bind(object_id)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::BoardPost));
        }

        let n =
            sqlx::query("UPDATE ap_timeline SET content = ? WHERE ap_id = ? AND author_uri = ?")
                .bind(content)
                .bind(object_id)
                .bind(actor_uri)
                .execute(pool)
                .await?
                .rows_affected();
        if n > 0 {
            return Ok(Some(Target::Status));
        }

        let n = sqlx::query(
            "UPDATE ap_board_posts SET subject = ?, content = ? WHERE ap_id = ? \
             AND (author_uri = ? OR group_uri = ?)",
        )
        .bind(subject)
        .bind(content)
        .bind(object_id)
        .bind(actor_uri)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::MirroredPost));
        }

        let n = sqlx::query(
            "UPDATE mail SET subject = ?, body = ? WHERE ap_id = ? \
             AND from_id IN (SELECT id FROM users WHERE actor_uri = ?)",
        )
        .bind(subject)
        .bind(content)
        .bind(object_id)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Some(Target::Mail));
        }
        Ok(None)
    }

    /// Delete a mirrored post on a board's relayed instruction — the
    /// `Announce{Delete}` path (#133).
    ///
    /// Deliberately much narrower than [`delete`]. A relayed activity is signed
    /// by the *Group*, not by the author, so the Group's signature alone would
    /// let any board we follow withdraw anything it names. Two conditions are
    /// required instead:
    ///
    /// 1. the row was announced by **this** Group (`group_uri`), so a board can
    ///    only act on content it actually hosts; and
    /// 2. the inner activity's actor is either the post's author or the Group
    ///    itself — a board may moderate its own content, but it can't forge a
    ///    withdrawal on behalf of a third party.
    ///
    /// Only `ap_board_posts` is reachable. Statuses, DMs and our own boards'
    /// `messages` are not a board's to withdraw, so they are never touched here
    /// — see [`announced_hits_local_content`].
    pub async fn delete_announced(
        pool: &SqlitePool,
        group_uri: &str,
        actor_uri: &str,
        object_id: &str,
    ) -> Result<bool> {
        let n = sqlx::query(
            "DELETE FROM ap_board_posts WHERE ap_id = ? AND group_uri = ? \
             AND (author_uri = ? OR group_uri = ?)",
        )
        .bind(object_id)
        .bind(group_uri)
        .bind(actor_uri)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Apply a board's relayed edit to a mirrored post. Same authorization rules
    /// as [`delete_announced`].
    pub async fn update_announced(
        pool: &SqlitePool,
        group_uri: &str,
        actor_uri: &str,
        object_id: &str,
        subject: &str,
        content: &str,
    ) -> Result<bool> {
        let n = sqlx::query(
            "UPDATE ap_board_posts SET subject = ?, content = ? WHERE ap_id = ? AND group_uri = ? \
             AND (author_uri = ? OR group_uri = ?)",
        )
        .bind(subject)
        .bind(content)
        .bind(object_id)
        .bind(group_uri)
        .bind(actor_uri)
        .bind(actor_uri)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Whether a relayed activity named content we're the authority for: a post
    /// on one of our own boards, or a status/DM sent directly to us.
    ///
    /// A remote board has no standing over any of these, so the caller refuses
    /// the activity — but a refusal worth logging is worth distinguishing from
    /// "we've never heard of this object", which is the far more common case.
    pub async fn announced_hits_local_content(pool: &SqlitePool, object_id: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM messages   WHERE ap_id = ?) \
                  + (SELECT COUNT(*) FROM ap_timeline WHERE ap_id = ?) \
                  + (SELECT COUNT(*) FROM mail        WHERE ap_id = ?)",
        )
        .bind(object_id)
        .bind(object_id)
        .bind(object_id)
        .fetch_one(pool)
        .await?;
        Ok(n > 0)
    }
}

/// Inbound reports (`Flag`) and after-the-fact cleanup (#112).
pub mod moderation {
    use super::*;
    use crate::util::now_unix;

    /// A report a remote instance sent us.
    #[derive(Debug, Clone, sqlx::FromRow)]
    pub struct Report {
        pub id: i64,
        pub reporter_handle: String,
        pub reporter_uri: String,
        /// Reported object URIs, one per line.
        pub objects: String,
        pub content: String,
        pub created_at: i64,
        pub resolved_at: Option<i64>,
    }

    /// Record an inbound report. Idempotent on the `Flag`'s own id, so a
    /// redelivered report doesn't pile up. Returns whether it was new.
    pub async fn record_report(
        pool: &SqlitePool,
        ap_id: &str,
        reporter_uri: &str,
        reporter_handle: &str,
        objects: &str,
        content: &str,
    ) -> Result<bool> {
        let n = sqlx::query(
            "INSERT INTO ap_reports \
               (ap_id, reporter_uri, reporter_handle, objects, content, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(ap_id) DO NOTHING",
        )
        .bind(ap_id)
        .bind(reporter_uri)
        .bind(reporter_handle)
        .bind(objects)
        .bind(content)
        .bind(now_unix())
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Reports for an operator to read — open ones unless `include_resolved`.
    pub async fn reports(
        pool: &SqlitePool,
        include_resolved: bool,
        limit: i64,
    ) -> Result<Vec<Report>> {
        let sql = if include_resolved {
            "SELECT id, reporter_handle, reporter_uri, objects, content, created_at, resolved_at \
             FROM ap_reports ORDER BY id DESC LIMIT ?"
        } else {
            "SELECT id, reporter_handle, reporter_uri, objects, content, created_at, resolved_at \
             FROM ap_reports WHERE resolved_at IS NULL ORDER BY id DESC LIMIT ?"
        };
        Ok(sqlx::query_as::<_, Report>(sql)
            .bind(limit)
            .fetch_all(pool)
            .await?)
    }

    /// Mark a report handled. Returns whether it existed and was open.
    pub async fn resolve_report(pool: &SqlitePool, id: i64) -> Result<bool> {
        let n = sqlx::query(
            "UPDATE ap_reports SET resolved_at = ? WHERE id = ? AND resolved_at IS NULL",
        )
        .bind(now_unix())
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// How many reports are open (operator visibility).
    pub async fn open_report_count(pool: &SqlitePool) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM ap_reports WHERE resolved_at IS NULL")
                .fetch_one(pool)
                .await?,
        )
    }

    /// Content purged from one domain, by store.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Purged {
        pub board_posts: u64,
        pub statuses: u64,
        pub mirrored_posts: u64,
        pub mail: u64,
    }

    /// Delete everything a domain sent us.
    ///
    /// **Blocking a peer is not retroactive** — it stops what arrives next and
    /// leaves what already arrived in place. This is the explicit tool for the
    /// second half, kept separate so removing content is always a deliberate act
    /// rather than a silent side effect of a policy change.
    pub async fn purge_domain(pool: &SqlitePool, domain: &str) -> Result<Purged> {
        let domain = domain.trim().to_ascii_lowercase();
        // Matches an actor URI's host: `https://host/...` or `https://host`.
        let host_prefix = format!("%//{domain}/%");
        let host_exact = format!("%//{domain}");

        let board_posts = sqlx::query(
            "DELETE FROM messages WHERE author_id IN \
               (SELECT id FROM users WHERE is_remote = 1 AND lower(domain) = ?)",
        )
        .bind(&domain)
        .execute(pool)
        .await?
        .rows_affected();

        let statuses =
            sqlx::query("DELETE FROM ap_timeline WHERE author_uri LIKE ? OR author_uri LIKE ?")
                .bind(&host_prefix)
                .bind(&host_exact)
                .execute(pool)
                .await?
                .rows_affected();

        let mirrored_posts = sqlx::query(
            "DELETE FROM ap_board_posts WHERE group_uri LIKE ? OR group_uri LIKE ? \
             OR author_uri LIKE ? OR author_uri LIKE ?",
        )
        .bind(&host_prefix)
        .bind(&host_exact)
        .bind(&host_prefix)
        .bind(&host_exact)
        .execute(pool)
        .await?
        .rows_affected();

        let mail = sqlx::query(
            "DELETE FROM mail WHERE from_id IN \
               (SELECT id FROM users WHERE is_remote = 1 AND lower(domain) = ?)",
        )
        .bind(&domain)
        .execute(pool)
        .await?
        .rows_affected();

        Ok(Purged {
            board_posts,
            statuses,
            mirrored_posts,
            mail,
        })
    }
}

/// Split a fediverse handle into `(user, domain)`. Accepts `alice@host`,
/// `@alice@host`, and `acct:alice@host`.
pub fn split_handle(handle: &str) -> Option<(&str, &str)> {
    let h = handle
        .trim()
        .trim_start_matches("acct:")
        .trim_start_matches('@');
    let (user, domain) = h.split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    Some((user, domain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new("https://bbs.example.com")
    }

    #[test]
    fn uris_are_built_from_the_origin() {
        let o = origin();
        assert_eq!(o.host(), "bbs.example.com");
        assert_eq!(o.person("alice"), "https://bbs.example.com/u/alice");
        assert_eq!(
            o.person_inbox("alice"),
            "https://bbs.example.com/u/alice/inbox"
        );
        assert_eq!(
            o.person_outbox("alice"),
            "https://bbs.example.com/u/alice/outbox"
        );
        assert_eq!(o.shared_inbox(), "https://bbs.example.com/inbox");
        assert_eq!(o.group("rust"), "https://bbs.example.com/c/rust");
        assert_eq!(o.acct("alice"), "acct:alice@bbs.example.com");
    }

    #[test]
    fn host_survives_a_dev_origin_with_a_port() {
        // debug_insecure origins keep their port; the host must still parse.
        let o = Origin::new("http://localhost:8088");
        assert_eq!(o.host(), "localhost:8088");
        assert_eq!(o.person("alice"), "http://localhost:8088/u/alice");
    }

    #[test]
    fn handles_split_in_every_form_we_might_be_handed() {
        assert_eq!(
            split_handle("alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("@alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("acct:alice@remote.social"),
            Some(("alice", "remote.social"))
        );
        assert_eq!(
            split_handle("  alice@remote.social "),
            Some(("alice", "remote.social"))
        );

        for bad in ["alice", "", "@", "alice@", "@remote.social", "acct:"] {
            assert_eq!(split_handle(bad), None, "{bad:?} is not a handle");
        }
    }

    #[test]
    fn a_direct_message_is_addressed_and_mentioned_for_mastodon() {
        // Mastodon treats a Note as a DM only when every addressed actor is also
        // in `tag` as a Mention; `to` carries just the recipient, `cc` is empty.
        let create = super::objects::direct_message(
            "https://bbs.example.com/dm/7",
            "https://bbs.example.com/u/alice",
            "https://remote.social/users/bob",
            "bob@remote.social",
            "hi & bye",
            "the <body>",
            0,
        );
        let v = serde_json::to_value(&create).unwrap();
        assert_eq!(v["type"], "Create");
        assert_eq!(v["id"], "https://bbs.example.com/dm/7/activity");
        assert_eq!(v["actor"], "https://bbs.example.com/u/alice");
        assert_eq!(v["to"][0], "https://remote.social/users/bob");
        assert!(v["cc"].as_array().unwrap().is_empty(), "a DM is not public");

        let note = &v["object"];
        assert_eq!(note["type"], "Note");
        assert_eq!(note["id"], "https://bbs.example.com/dm/7");
        assert_eq!(note["to"][0], "https://remote.social/users/bob");
        assert!(
            note["cc"].as_array().unwrap().is_empty(),
            "never Public or followers"
        );
        // The Mention is what makes it 'direct' rather than 'limited'.
        assert_eq!(note["tag"][0]["type"], "Mention");
        assert_eq!(note["tag"][0]["href"], "https://remote.social/users/bob");
        assert_eq!(note["tag"][0]["name"], "@bob@remote.social");
        // Subject becomes a bold first line; both subject and body are escaped.
        assert_eq!(
            note["content"],
            "<p><b>hi &amp; bye</b></p><p>the &lt;body&gt;</p>"
        );
    }

    #[test]
    fn slugs_are_uri_safe() {
        assert_eq!(slugify("General", 1), "general");
        assert_eq!(slugify("Rust  &  Cargo!", 1), "rust-cargo");
        assert_eq!(slugify("  Off-Topic  ", 1), "off-topic");
        assert_eq!(slugify("日本語", 1), "board-1"); // no ASCII → id fallback
        assert_eq!(slugify("", 42), "board-42");
        assert_eq!(slugify("!!!", 5), "board-5");
    }

    #[test]
    fn a_board_post_announces_as_a_group_page() {
        let o = Origin::new("https://bbs.example.com");
        let page = super::objects::board_page(
            &o,
            "rust",
            "https://bbs.example.com/p/9",
            "https://bbs.example.com/u/alice",
            "Hello & <world>",
            "the body & more",
            0,
        );
        let author = page.attributed_to.clone();
        let announce = super::objects::board_announce(&o, "rust", &author, 1, page);
        let v = serde_json::to_value(&announce).unwrap();

        // The Group is the actor of the Announce; the inner Create keeps the author.
        assert_eq!(v["type"], "Announce");
        assert_eq!(v["actor"], "https://bbs.example.com/c/rust");
        assert_eq!(v["cc"][0], "https://bbs.example.com/c/rust/followers");
        assert_eq!(v["object"]["type"], "Create");
        assert_eq!(v["object"]["actor"], "https://bbs.example.com/u/alice");
        assert_eq!(v["object"]["audience"], "https://bbs.example.com/c/rust");

        let page = &v["object"]["object"];
        assert_eq!(page["type"], "Page");
        assert_eq!(page["id"], "https://bbs.example.com/p/9");
        assert_eq!(page["name"], "Hello & <world>", "subject is plain text");
        assert_eq!(
            page["content"], "<p>the body &amp; more</p>",
            "body is HTML-escaped"
        );
        assert_eq!(page["audience"], "https://bbs.example.com/c/rust");
        assert_eq!(
            page["to"][0],
            "https://www.w3.org/ns/activitystreams#Public"
        );
    }

    #[test]
    fn a_direct_message_without_a_subject_is_just_the_body() {
        let create = super::objects::direct_message(
            "https://bbs.example.com/dm/1",
            "https://bbs.example.com/u/alice",
            "https://remote.social/users/bob",
            "@bob@remote.social",
            "   ",
            "hello",
            0,
        );
        let v = serde_json::to_value(&create).unwrap();
        assert_eq!(v["object"]["content"], "<p>hello</p>");
        // A leading @ in the handle isn't doubled in the Mention name.
        assert_eq!(v["object"]["tag"][0]["name"], "@bob@remote.social");
    }

    mod content {
        use super::super::content::html_to_text;

        #[test]
        fn paragraphs_and_breaks_become_lines() {
            assert_eq!(html_to_text("<p>first</p><p>second</p>"), "first\nsecond");
            assert_eq!(html_to_text("a<br>b<br/>c"), "a\nb\nc");
        }

        #[test]
        fn entities_are_decoded() {
            assert_eq!(
                html_to_text("<p>a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39;</p>"),
                "a & b <c> \"d\" 'e'"
            );
            // Unknown entities are left as-is rather than mangled.
            assert_eq!(html_to_text("x &frobnicate; y"), "x &frobnicate; y");
        }

        #[test]
        fn links_keep_text_and_append_the_url_when_it_adds_something() {
            // A truncated visible label gains the real URL.
            assert_eq!(
                html_to_text(r#"see <a href="https://example.com/page">example.com</a>"#),
                "see example.com (https://example.com/page)",
            );
            // When the visible text already is the URL, don't duplicate it.
            assert_eq!(
                html_to_text(r#"<a href="https://example.com/">https://example.com/</a>"#),
                "https://example.com/",
            );
            // A hashtag/mention link keeps just its text.
            assert_eq!(
                html_to_text(r#"<a href="https://h.example/tags/rust">#rust</a>"#),
                "#rust (https://h.example/tags/rust)",
            );
        }

        #[test]
        fn images_degrade_to_alt_and_source() {
            assert_eq!(
                html_to_text(r#"<img alt="a cat" src="https://cdn.example/cat.png">"#),
                "[img: a cat] (https://cdn.example/cat.png)",
            );
            // No alt still records that an image was here, with its source.
            assert_eq!(
                html_to_text(r#"<img src="https://cdn.example/x.png">"#),
                "[img] (https://cdn.example/x.png)",
            );
        }

        #[test]
        fn unknown_tags_are_stripped_and_blank_lines_squeezed() {
            assert_eq!(
                html_to_text("<span class=\"h\">hi</span> <b>there</b>"),
                "hi there"
            );
            assert_eq!(
                html_to_text("<p>one</p><p></p><p></p><p>two</p>"),
                "one\ntwo"
            );
        }

        #[test]
        fn a_realistic_mastodon_status_flattens_sensibly() {
            let html = "<p>Hello <a href=\"https://mastodon.social/@bob\">@bob</a>! \
                        Check <a href=\"https://example.com/very/long/path\">this link</a></p>\
                        <p>Second paragraph.</p>";
            assert_eq!(
                html_to_text(html),
                "Hello @bob (https://mastodon.social/@bob)! Check this link (https://example.com/very/long/path)\nSecond paragraph.",
            );
        }
    }
}
