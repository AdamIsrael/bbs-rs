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

/// Format a Unix timestamp as an RFC 822 date, e.g.
/// `Tue, 08 Jul 2026 14:03:22 GMT` — what RSS 2.0 `pubDate` expects.
///
/// Hand-built for the same reason as [`fmt_rfc3339`]: it avoids the `time`
/// crate's `formatting` feature for one string. `from_unix_timestamp` is UTC,
/// so `GMT` is honest.
pub fn fmt_rfc822(ts: i64) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match OffsetDateTime::from_unix_timestamp(ts) {
        Ok(dt) => format!(
            "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
            DAYS[dt.weekday().number_days_from_monday() as usize],
            dt.day(),
            MONTHS[u8::from(dt.month()) as usize - 1],
            dt.year(),
            dt.hour(),
            dt.minute(),
            dt.second()
        ),
        Err(_) => "Thu, 01 Jan 1970 00:00:00 GMT".to_string(),
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

/// The subject for a reply: the parent's, prefixed with `Re: ` unless it
/// already starts with one.
///
/// Shared because two callers need the identical rule: the BBS compose screen,
/// and federation ingestion, which falls back to it when a remote reply `Note`
/// arrives with no `name` of its own (#139). A second copy would drift.
pub fn reply_subject(subject: &str) -> String {
    if subject.to_ascii_lowercase().starts_with("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}
