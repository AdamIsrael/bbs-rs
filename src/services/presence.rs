//! In-memory registry of live sessions, shared across all SSH connections.
//! Not persisted — it reflects currently-connected sessions only.
//!
//! Besides powering the "who's online" screen, it holds each session's event
//! sender so bans can kick active sessions: sending [`Event::Quit`] makes the
//! app loop exit, and the SSH shell wrapper then closes the channel. Both the
//! sender and `Event` are transport-agnostic, so this stays free of russh
//! types.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;

use crate::transport::Event;
use crate::util::now_unix;

/// A live session's entry.
#[derive(Clone)]
struct Session {
    username: String,
    ip: Option<String>,
    since: i64,
    tx: Sender<Event>,
}

/// Public view of a connected user (for the who's-online screen).
#[derive(Debug, Clone)]
pub struct OnlineUser {
    pub username: String,
    pub since: i64,
}

#[derive(Clone, Default)]
pub struct Presence {
    inner: Arc<RwLock<HashMap<usize, Session>>>,
    /// Session ids currently in the live chat room (#67). A subset of `inner`.
    chat: Arc<RwLock<HashSet<usize>>>,
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session as online.
    pub async fn join(
        &self,
        session_id: usize,
        username: String,
        ip: Option<String>,
        tx: Sender<Event>,
    ) {
        self.inner.write().await.insert(
            session_id,
            Session {
                username,
                ip,
                since: now_unix(),
                tx,
            },
        );
    }

    /// Remove a session (idempotent). Also drops it from the chat room, so a
    /// disconnect can't leave a ghost member behind.
    pub async fn leave(&self, session_id: usize) {
        self.inner.write().await.remove(&session_id);
        self.chat.write().await.remove(&session_id);
    }

    /// Add a session to the live chat room (#67).
    pub async fn chat_join(&self, session_id: usize) {
        self.chat.write().await.insert(session_id);
    }

    /// Remove a session from the chat room (idempotent).
    pub async fn chat_leave(&self, session_id: usize) {
        self.chat.write().await.remove(&session_id);
    }

    /// Deliver an event to every session currently in the chat room, returning
    /// how many received it.
    pub async fn chat_send(&self, event: Event) -> usize {
        let members = self.chat.read().await;
        let sessions = self.inner.read().await;
        let mut delivered = 0;
        for id in members.iter() {
            if let Some(s) = sessions.get(id)
                && s.tx.send(event.clone()).await.is_ok()
            {
                delivered += 1;
            }
        }
        delivered
    }

    /// Usernames of the sessions currently in the chat room, sorted and
    /// de-duplicated (a user may have more than one session in the room).
    pub async fn chat_roster(&self) -> Vec<String> {
        let members = self.chat.read().await;
        let sessions = self.inner.read().await;
        let mut names: Vec<String> = members
            .iter()
            .filter_map(|id| sessions.get(id).map(|s| s.username.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Snapshot of currently-connected users, sorted by name.
    pub async fn list(&self) -> Vec<OnlineUser> {
        let mut users: Vec<OnlineUser> = self
            .inner
            .read()
            .await
            .values()
            .map(|s| OnlineUser {
                username: s.username.clone(),
                since: s.since,
            })
            .collect();
        users.sort_by(|a, b| a.username.cmp(&b.username));
        users
    }

    /// Deliver an event to every live session belonging to `username` (a user
    /// may be connected more than once). Returns how many sessions received it,
    /// so a caller can tell "delivered" from "that user isn't online". Used by
    /// user paging (#68) to fan an [`Event::Paged`] to the target.
    pub async fn send_to_user(&self, username: &str, event: Event) -> usize {
        let mut delivered = 0;
        for session in self.inner.read().await.values() {
            if session.username == username && session.tx.send(event.clone()).await.is_ok() {
                delivered += 1;
            }
        }
        delivered
    }

    /// A snapshot of `(session_id, username, connected_since)` for every live
    /// session — what the time-limit sweeper (#75) needs to compute how long
    /// each session has been on.
    pub async fn sessions_snapshot(&self) -> Vec<(usize, String, i64)> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(id, s)| (*id, s.username.clone(), s.since))
            .collect()
    }

    /// Deliver an event to one session. Returns whether it was still connected.
    pub async fn send_to(&self, session_id: usize, event: Event) -> bool {
        match self.inner.read().await.get(&session_id) {
            Some(s) => s.tx.send(event).await.is_ok(),
            None => false,
        }
    }

    /// Deliver an event to *every* live session, returning how many received
    /// it. Used by sysop broadcast (#69) to push a notice to the whole board.
    pub async fn broadcast(&self, event: Event) -> usize {
        let mut delivered = 0;
        for session in self.inner.read().await.values() {
            if session.tx.send(event.clone()).await.is_ok() {
                delivered += 1;
            }
        }
        delivered
    }

    /// Ask every session whose user or IP is banned to quit. The app loop
    /// exits on [`Event::Quit`] and the SSH wrapper closes the channel.
    /// Returns the number of sessions signalled.
    pub async fn kick(
        &self,
        banned_users: &HashSet<String>,
        banned_ips: &HashSet<String>,
    ) -> usize {
        let mut kicked = 0;
        for session in self.inner.read().await.values() {
            let user_banned = banned_users.contains(&session.username);
            let ip_banned = session
                .ip
                .as_ref()
                .is_some_and(|ip| banned_ips.contains(ip));
            if user_banned || ip_banned {
                let _ = session.tx.send(Event::Quit).await;
                kicked += 1;
            }
        }
        kicked
    }
}
