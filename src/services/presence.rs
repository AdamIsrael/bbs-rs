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

    /// Remove a session (idempotent).
    pub async fn leave(&self, session_id: usize) {
        self.inner.write().await.remove(&session_id);
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
