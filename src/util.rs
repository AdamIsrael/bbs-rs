//! Small shared helpers.

use time::OffsetDateTime;

/// Current time as a Unix timestamp (seconds). Timestamps are stored in the DB
/// as plain integers to avoid pulling in sqlx's chrono/time column mapping.
pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// Format a Unix timestamp for display, e.g. `2026-07-08 14:03`.
pub fn fmt_time(ts: i64) -> String {
    match OffsetDateTime::from_unix_timestamp(ts) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute()
        ),
        Err(_) => ts.to_string(),
    }
}
