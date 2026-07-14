//! Full-text search over board messages, backed by the `messages_fts` FTS5
//! index. Results are filtered to boards the searcher may read.

use sqlx::FromRow;
use sqlx::sqlite::SqlitePool;

use crate::error::Result;
use crate::services::role_rank;

/// Maximum number of search hits returned / shown.
pub const SEARCH_LIMIT: i64 = 50;

/// A search result: enough of the matched message to list it and jump to it.
#[derive(Debug, Clone, FromRow)]
pub struct SearchHit {
    pub id: i64,
    pub board_id: i64,
    pub board_name: String,
    pub author_name: String,
    pub subject: String,
    pub body: String,
    pub created_at: i64,
}

/// Turn a user's raw search string into a safe FTS5 MATCH expression: each
/// whitespace-separated term is quoted (so FTS operators/punctuation can't
/// cause a syntax error) and the terms are implicitly AND-ed. Returns `None`
/// when the input has no searchable tokens.
fn fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// Search board messages for `query`, best matches first, limited to `limit`
/// rows and to boards readable by `role`. An empty/blank query yields no hits.
pub async fn search_messages(
    pool: &SqlitePool,
    role: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<SearchHit>> {
    let Some(fts) = fts_query(query) else {
        return Ok(Vec::new());
    };
    // Filter by read ACL in SQL (so LIMIT counts only visible rows). The CASE
    // mirrors `services::role_rank` (guest=0, user=1, admin=2, unknown=0).
    let hits = sqlx::query_as::<_, SearchHit>(
        "SELECT m.id, m.board_id, b.name AS board_name, u.username AS author_name, \
         m.subject, m.body, m.created_at \
         FROM messages_fts f \
         JOIN messages m ON m.id = f.rowid \
         JOIN users u ON u.id = m.author_id \
         JOIN boards b ON b.id = m.board_id \
         WHERE f.messages_fts MATCH ? \
         AND (CASE b.min_read_role \
                WHEN 'guest' THEN 0 WHEN 'user' THEN 1 WHEN 'admin' THEN 2 ELSE 0 END) <= ? \
         ORDER BY f.rank \
         LIMIT ?",
    )
    .bind(fts)
    .bind(role_rank(role) as i64)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(hits)
}
