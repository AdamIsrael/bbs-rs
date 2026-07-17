//! Small shared helpers.

use time::OffsetDateTime;

/// Current time as a Unix timestamp (seconds). Timestamps are stored in the DB
/// as plain integers to avoid pulling in sqlx's chrono/time column mapping.
pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// Format a Unix timestamp as RFC 3339 UTC, e.g. `2026-07-08T14:03:22Z` — what
/// ActivityStreams `published` expects.
///
/// Built by hand for the same reason as [`fmt_time`]: it avoids enabling the
/// `time` crate's `formatting` feature just for one string.
pub fn fmt_rfc3339(ts: i64) -> String {
    match OffsetDateTime::from_unix_timestamp(ts) {
        // `from_unix_timestamp` yields UTC, so the `Z` offset is honest.
        Ok(dt) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        ),
        Err(_) => "1970-01-01T00:00:00Z".to_string(),
    }
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
