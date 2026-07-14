//! Board-wide statistics: totals, a top-posters leaderboard, and the most
//! recent callers. All simple aggregations over `users` / `messages` / `logins`
//! for a read-only stats screen.

use sqlx::sqlite::SqlitePool;

use crate::error::Result;

/// How many rows the leaderboard / recent-callers lists show.
pub const LIST_LIMIT: i64 = 10;

/// One entry on the top-posters leaderboard.
#[derive(Debug, Clone)]
pub struct PosterStat {
    pub username: String,
    pub posts: i64,
}

/// One recent caller: a user's most recent successful login.
#[derive(Debug, Clone)]
pub struct CallerStat {
    pub username: String,
    pub at: i64,
}

/// A snapshot of board-wide stats for the stats screen.
#[derive(Debug, Clone)]
pub struct Stats {
    pub total_users: i64,
    pub total_posts: i64,
    /// Total successful logins ever recorded ("calls").
    pub total_calls: i64,
    pub top_posters: Vec<PosterStat>,
    pub recent_callers: Vec<CallerStat>,
}

/// Gather all stats in one shot. `limit` bounds the leaderboard / caller lists.
pub async fn gather(pool: &SqlitePool, limit: i64) -> Result<Stats> {
    let total_users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    let total_posts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await?;
    let total_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM logins WHERE success = 1")
        .fetch_one(pool)
        .await?;

    let top_posters = sqlx::query_as::<_, (String, i64)>(
        "SELECT u.username, COUNT(*) AS n \
         FROM messages m JOIN users u ON u.id = m.author_id \
         GROUP BY m.author_id ORDER BY n DESC, u.username LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(username, posts)| PosterStat { username, posts })
    .collect();

    // Most recent successful login per user, newest first.
    let recent_callers = sqlx::query_as::<_, (String, i64)>(
        "SELECT username, MAX(created_at) AS t \
         FROM logins WHERE success = 1 \
         GROUP BY username ORDER BY t DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(username, at)| CallerStat { username, at })
    .collect();

    Ok(Stats {
        total_users,
        total_posts,
        total_calls,
        top_posters,
        recent_callers,
    })
}
