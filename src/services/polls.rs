//! Polls / voting booth (#72). A poll has a question and two or more options;
//! each user casts at most one vote per poll, changeable while the poll is open.
//! Closing a poll freezes voting but keeps results visible.
//!
//! Guests (a shared account) can read polls but not create or vote. Creation is
//! throttled through the same `[limits]` window as board posts.

use sqlx::SqlitePool;

use crate::config::Limits;
use crate::db::models::User;
use crate::error::{AppError, Result};
use crate::services::{enforce_len, enforce_rate};

/// The most options a single poll may offer.
pub const MAX_OPTIONS: usize = 8;

/// A poll summary for the list view.
#[derive(Debug, Clone)]
pub struct Poll {
    pub id: i64,
    pub author_name: String,
    pub question: String,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    pub total_votes: i64,
}

impl Poll {
    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some()
    }
}

/// One option with its vote tally.
#[derive(Debug, Clone)]
pub struct PollOption {
    pub id: i64,
    pub position: i64,
    pub label: String,
    pub votes: i64,
}

/// A poll plus its options and the viewer's own vote (the chosen `option_id`).
#[derive(Debug, Clone)]
pub struct PollDetail {
    pub poll: Poll,
    pub options: Vec<PollOption>,
    pub my_vote: Option<i64>,
}

/// Create a poll from a question and its option labels. Guests can't; the
/// question and at least two non-blank options are required (blanks are
/// dropped, capped at [`MAX_OPTIONS`]); creation is rate-limited like posts.
/// Returns the new poll id.
pub async fn create_poll(
    pool: &SqlitePool,
    author: &User,
    question: &str,
    options: &[String],
    limits: &Limits,
    now: i64,
) -> Result<i64> {
    if author.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let question = question.trim();
    let opts: Vec<&str> = options
        .iter()
        .map(|o| o.trim())
        .filter(|o| !o.is_empty())
        .take(MAX_OPTIONS)
        .collect();
    if question.is_empty() || opts.len() < 2 {
        return Err(AppError::PollInvalid);
    }
    // Bound the text like a post subject, so a poll can't carry huge fields.
    enforce_len("Question", question, limits.max_subject_chars)?;
    for o in &opts {
        enforce_len("Option", o, limits.max_subject_chars)?;
    }
    if !author.is_admin()
        && let Some(since) = limits.window_start(now)
    {
        let count = recent_poll_count(pool, author.id, since).await?;
        enforce_rate(count, limits.max_posts)?;
    }

    let mut tx = pool.begin().await?;
    let poll_id: i64 = sqlx::query_scalar(
        "INSERT INTO polls (author_id, question, created_at) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(author.id)
    .bind(question)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    for (i, label) in opts.iter().enumerate() {
        sqlx::query("INSERT INTO poll_options (poll_id, position, label) VALUES (?, ?, ?)")
            .bind(poll_id)
            .bind(i as i64)
            .bind(*label)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(poll_id)
}

async fn recent_poll_count(pool: &SqlitePool, author_id: i64, since: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM polls WHERE author_id = ? AND created_at >= ?")
            .bind(author_id)
            .bind(since)
            .fetch_one(pool)
            .await?,
    )
}

/// Cast (or change) `user`'s vote on an open poll. Guests can't vote; a closed
/// or missing poll is rejected, as is an `option_id` that isn't one of the
/// poll's options. Re-voting replaces the previous choice.
pub async fn vote(
    pool: &SqlitePool,
    poll_id: i64,
    user: &User,
    option_id: i64,
    now: i64,
) -> Result<()> {
    if user.is_guest() {
        return Err(AppError::GuestNotAllowed);
    }
    let closed_at: Option<i64> = sqlx::query_scalar("SELECT closed_at FROM polls WHERE id = ?")
        .bind(poll_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)?;
    if closed_at.is_some() {
        return Err(AppError::PollClosed);
    }
    // The option must belong to this poll (guards a forged option id).
    let ok: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM poll_options WHERE id = ? AND poll_id = ?")
            .bind(option_id)
            .bind(poll_id)
            .fetch_optional(pool)
            .await?;
    if ok.is_none() {
        return Err(AppError::NotFound);
    }
    sqlx::query(
        "INSERT INTO poll_votes (poll_id, option_id, user_id, created_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(poll_id, user_id) DO UPDATE SET option_id = excluded.option_id, \
         created_at = excluded.created_at",
    )
    .bind(poll_id)
    .bind(option_id)
    .bind(user.id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Recent polls (newest first), each with its author and total vote count.
pub async fn list_polls(pool: &SqlitePool, limit: i64) -> Result<Vec<Poll>> {
    let rows: Vec<(i64, String, String, i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT p.id, u.username, p.question, p.created_at, p.closed_at, \
                (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id = p.id) \
         FROM polls p JOIN users u ON u.id = p.author_id \
         ORDER BY p.created_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, author_name, question, created_at, closed_at, total_votes)| Poll {
                id,
                author_name,
                question,
                created_at,
                closed_at,
                total_votes,
            },
        )
        .collect())
}

/// A poll with its options (in order, with tallies) and `user_id`'s own vote.
/// Returns `NotFound` if the poll doesn't exist.
pub async fn get_poll(pool: &SqlitePool, poll_id: i64, user_id: i64) -> Result<PollDetail> {
    let row: Option<(i64, String, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT p.id, u.username, p.question, p.created_at, p.closed_at \
         FROM polls p JOIN users u ON u.id = p.author_id WHERE p.id = ?",
    )
    .bind(poll_id)
    .fetch_optional(pool)
    .await?;
    let (id, author_name, question, created_at, closed_at) = row.ok_or(AppError::NotFound)?;

    let options: Vec<PollOption> = sqlx::query_as(
        "SELECT o.id, o.position, o.label, \
                (SELECT COUNT(*) FROM poll_votes v WHERE v.option_id = o.id) \
         FROM poll_options o WHERE o.poll_id = ? ORDER BY o.position",
    )
    .bind(poll_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(id, position, label, votes)| PollOption {
        id,
        position,
        label,
        votes,
    })
    .collect();

    let total_votes = options.iter().map(|o| o.votes).sum();
    let my_vote: Option<i64> =
        sqlx::query_scalar("SELECT option_id FROM poll_votes WHERE poll_id = ? AND user_id = ?")
            .bind(poll_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;

    Ok(PollDetail {
        poll: Poll {
            id,
            author_name,
            question,
            created_at,
            closed_at,
            total_votes,
        },
        options,
        my_vote,
    })
}

/// Close a poll (set `closed_at`), stopping further votes. Returns whether a
/// row changed (false if already closed or missing). Caller gates who may close.
pub async fn close_poll(pool: &SqlitePool, poll_id: i64, now: i64) -> Result<bool> {
    let affected = sqlx::query("UPDATE polls SET closed_at = ? WHERE id = ? AND closed_at IS NULL")
        .bind(now)
        .bind(poll_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// The author id of a poll (for a creator-or-admin permission check).
pub async fn author_of(pool: &SqlitePool, poll_id: i64) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT author_id FROM polls WHERE id = ?")
            .bind(poll_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Delete a poll and its options and votes. Returns whether it existed. The
/// connection doesn't enforce the FK cascade, so the children are removed
/// explicitly.
pub async fn delete_poll(pool: &SqlitePool, poll_id: i64) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM poll_votes WHERE poll_id = ?")
        .bind(poll_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM poll_options WHERE poll_id = ?")
        .bind(poll_id)
        .execute(&mut *tx)
        .await?;
    let affected = sqlx::query("DELETE FROM polls WHERE id = ?")
        .bind(poll_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(affected > 0)
}
