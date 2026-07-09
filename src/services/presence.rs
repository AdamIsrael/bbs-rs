//! In-memory "who's online" registry shared across all SSH connections.
//! Not persisted — it reflects live sessions only.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::util::now_unix;

#[derive(Debug, Clone)]
pub struct OnlineUser {
    pub username: String,
    pub since: i64,
}

#[derive(Clone, Default)]
pub struct Presence {
    inner: Arc<RwLock<HashMap<usize, OnlineUser>>>,
}

impl Presence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session as online.
    pub async fn join(&self, session_id: usize, username: String) {
        self.inner.write().await.insert(
            session_id,
            OnlineUser {
                username,
                since: now_unix(),
            },
        );
    }

    /// Remove a session (idempotent).
    pub async fn leave(&self, session_id: usize) {
        self.inner.write().await.remove(&session_id);
    }

    /// Snapshot of currently-connected users, sorted by name.
    pub async fn list(&self) -> Vec<OnlineUser> {
        let mut users: Vec<OnlineUser> = self.inner.read().await.values().cloned().collect();
        users.sort_by(|a, b| a.username.cmp(&b.username));
        users
    }
}
