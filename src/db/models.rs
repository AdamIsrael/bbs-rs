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

/// A banned IP address.
#[derive(Debug, Clone, FromRow)]
pub struct IpBan {
    pub ip: String,
    pub reason: String,
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
