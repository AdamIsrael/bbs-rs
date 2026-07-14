//! Row types mapped from SQLite via `sqlx::FromRow`.
//!
//! Some columns (e.g. foreign-key ids, `created_at`) are mapped for
//! completeness/future use even though the current UI doesn't display them.
#![allow(dead_code)]

use sqlx::FromRow;

#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub created_at: i64,
    /// Unix timestamp of the ban, or `None` if the account is not banned.
    pub banned_at: Option<i64>,
}

impl User {
    /// The shared limited account cannot post, mail, or receive mail.
    pub fn is_guest(&self) -> bool {
        self.role == "guest"
    }

    /// Operators who may manage users (list, ban/unban, view logins).
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }

    /// A banned account is refused at login.
    pub fn is_banned(&self) -> bool {
        self.banned_at.is_some()
    }
}

/// A sysop bulletin (dated announcement shown after login).
#[derive(Debug, Clone, FromRow)]
pub struct Bulletin {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: i64,
}

/// A public one-line "graffiti wall" post, joined with its author's name.
#[derive(Debug, Clone, FromRow)]
pub struct Oneliner {
    pub id: i64,
    pub author_id: i64,
    pub author_name: String,
    pub body: String,
    pub created_at: i64,
}

/// An SSH public key registered to a user for public-key authentication.
#[derive(Debug, Clone, FromRow)]
pub struct UserKey {
    pub id: i64,
    pub user_id: i64,
    pub algorithm: String,
    /// SHA256 fingerprint (`SHA256:…`), used for auth matching and display.
    pub fingerprint: String,
    /// Canonical OpenSSH encoding (algorithm + base64, no comment).
    pub public_key: String,
    /// Free-text label (defaults to the key's comment).
    pub label: String,
    pub created_at: i64,
}

/// A banned IP address. `expires_at` is `None` for a permanent ban.
#[derive(Debug, Clone, FromRow)]
pub struct IpBan {
    pub ip: String,
    pub reason: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// A file area: a named download area with a read/write role ACL.
#[derive(Debug, Clone, FromRow)]
pub struct FileArea {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub min_read_role: String,
    pub min_write_role: String,
    pub created_at: i64,
}

impl FileArea {
    /// Whether a viewer with `role` may list/download from this area.
    pub fn can_read(&self, role: &str) -> bool {
        crate::services::role_rank(role) >= crate::services::role_rank(&self.min_read_role)
    }

    /// Whether a viewer with `role` may upload to this area.
    pub fn can_write(&self, role: &str) -> bool {
        crate::services::role_rank(role) >= crate::services::role_rank(&self.min_write_role)
    }
}

/// A file in an area, joined with its uploader's name (`uploader_name`).
#[derive(Debug, Clone, FromRow)]
pub struct FileEntry {
    pub id: i64,
    pub area_id: i64,
    pub uploader_id: i64,
    pub uploader_name: String,
    pub filename: String,
    pub description: String,
    pub size: i64,
    pub storage_path: String,
    pub downloads: i64,
    pub created_at: i64,
}

/// A recorded login attempt (successful or not).
#[derive(Debug, Clone, FromRow)]
pub struct Login {
    pub id: i64,
    pub username: String,
    pub ip: Option<String>,
    pub success: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct Board {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// Minimum role required to read this board (`guest` | `user` | `admin`).
    pub min_read_role: String,
    /// Minimum role required to post to this board.
    pub min_write_role: String,
    /// When set, the board is frozen: no new posts (until an admin unlocks it).
    pub locked: bool,
}

impl Board {
    /// Whether a viewer with `role` may read this board.
    pub fn can_read(&self, role: &str) -> bool {
        crate::services::role_rank(role) >= crate::services::role_rank(&self.min_read_role)
    }

    /// Whether a viewer with `role` may post to this board (ignoring the lock,
    /// which is checked separately so the UI can explain *why*).
    pub fn can_write(&self, role: &str) -> bool {
        crate::services::role_rank(role) >= crate::services::role_rank(&self.min_write_role)
    }
}

/// A board message joined with its author's name (`author_name`).
#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub id: i64,
    pub board_id: i64,
    pub author_id: i64,
    pub author_name: String,
    pub subject: String,
    pub body: String,
    pub created_at: i64,
    /// Pinned messages sort to the top of a board (moderator highlight).
    pub pinned: bool,
    /// The message this one replies to, or `None` for a top-level post.
    pub parent_id: Option<i64>,
}

/// A private message joined with the sender's name (`from_name`).
#[derive(Debug, Clone, FromRow)]
pub struct Mail {
    pub id: i64,
    pub from_id: i64,
    pub to_id: i64,
    pub from_name: String,
    pub subject: String,
    pub body: String,
    pub created_at: i64,
    pub read_at: Option<i64>,
}
