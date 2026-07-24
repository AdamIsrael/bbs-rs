//! Public RSS/Atom feeds for guest-readable boards (#100).
//!
//! The web server is an HTTP surface, so the terminal/auth mismatch that killed
//! the original RSS idea (#16) doesn't apply here — but **only** to boards a
//! guest could already read. A board whose `min_read_role` is anything above
//! `guest` never appears in a feed and 404s identically to a board that doesn't
//! exist, so a feed URL can't be used to probe for restricted boards.
//!
//! Read-only and unauthenticated. Atom is the default; `?format=rss` serves
//! RSS 2.0. Both are hand-rendered with strict XML escaping — the same
//! dependency-light approach the rest of the crate takes.

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::db::models::Board;
use crate::services::boards;
use crate::util::{fmt_rfc822, fmt_rfc3339};

use super::WebState;

/// The most recent posts a single feed carries. Enough for a reader to catch up
/// without turning a busy board's feed into an unbounded document.
const FEED_LIMIT: i64 = 50;

const ATOM_CONTENT_TYPE: &str = "application/atom+xml; charset=utf-8";
const RSS_CONTENT_TYPE: &str = "application/rss+xml; charset=utf-8";

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    /// `rss` selects RSS 2.0; anything else (including absent) is Atom.
    #[serde(default)]
    format: String,
}

/// Percent-encode a board name for use as a URL path segment. Deliberately
/// tiny — the crate has no URL dependency of its own — and conservative: only
/// RFC 3986 unreserved characters pass through untouched.
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Escape text for insertion into XML character data or a double-quoted
/// attribute. Covers all five predefined entities so the output is valid in
/// both positions.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Strip control characters (except tab/newline/carriage return):
            // they are illegal in XML 1.0 and would make the whole feed
            // unparseable, and a board post has no business carrying them.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// The base URL for absolute links in a feed — the browser frontend's own
/// address (`Web::connect_url`), not the federation origin, since these links
/// are informational and a board need not federate to publish a feed.
fn base_url(state: &WebState) -> String {
    state.config.load().web.connect_url()
}

/// `GET /feed` — a small HTML index of the boards that have public feeds, so a
/// human can discover the URLs. 404 when feeds are disabled.
pub async fn index(State(state): State<WebState>) -> Response {
    if !state.config.load().web.feeds {
        return StatusCode::NOT_FOUND.into_response();
    }
    let boards = match boards::list_readable_boards(&state.pool, "guest").await {
        Ok(bs) => bs,
        Err(e) => {
            tracing::error!("feed index: listing boards: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let base = base_url(&state);
    let name = state.config.load().bbs.name.clone();
    let mut body = String::new();
    body.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    body.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    body.push_str(&format!("<title>{} — feeds</title>", xml_escape(&name)));
    body.push_str("</head><body>");
    body.push_str(&format!("<h1>{} — board feeds</h1>", xml_escape(&name)));
    if boards.is_empty() {
        body.push_str("<p>No public boards are available.</p>");
    } else {
        body.push_str("<ul>");
        for b in &boards {
            let path = encode_path(&b.name);
            body.push_str(&format!(
                "<li><a href=\"{base}/feed/{path}\">{}</a>",
                xml_escape(&b.name)
            ));
            if !b.description.trim().is_empty() {
                body.push_str(&format!(" — {}", xml_escape(&b.description)));
            }
            body.push_str(&format!(
                " (<a href=\"{base}/feed/{path}\">Atom</a>, \
                 <a href=\"{base}/feed/{path}?format=rss\">RSS</a>)</li>"
            ));
        }
        body.push_str("</ul>");
    }
    body.push_str("</body></html>");
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

/// `GET /feed/{board}` — the board's recent posts as Atom (or RSS with
/// `?format=rss`). 404 unless feeds are enabled and the board is guest-readable.
pub async fn board(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Query(q): Query<FeedQuery>,
) -> Response {
    if !state.config.load().web.feeds {
        return StatusCode::NOT_FOUND.into_response();
    }
    let board = match boards::find_board_by_name(&state.pool, &name).await {
        // The ACL check and the existence check collapse into one 404: a
        // restricted board is indistinguishable from a missing one.
        Ok(Some(b)) if b.can_read("guest") => b,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("feed for {name:?}: board lookup: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let (mut messages, _) = match boards::board_posts(&state.pool, board.id, FEED_LIMIT).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("feed for {name:?}: loading posts: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // board_posts returns newest-first already; keep it — that's feed order.
    messages.truncate(FEED_LIMIT as usize);

    let base = base_url(&state);
    if q.format.eq_ignore_ascii_case("rss") {
        let body = render_rss(&base, &board, &messages);
        ([(header::CONTENT_TYPE, RSS_CONTENT_TYPE)], body).into_response()
    } else {
        let body = render_atom(&base, &board, &messages);
        ([(header::CONTENT_TYPE, ATOM_CONTENT_TYPE)], body).into_response()
    }
}

/// A stable, absolute entry id for a message. Not required to resolve to a page
/// (the frontend is a terminal), but it is a valid IRI and unique per post, so
/// readers dedupe correctly across fetches.
fn entry_id(base: &str, board: &Board, msg_id: i64) -> String {
    format!("{base}/feed/{}#post-{msg_id}", encode_path(&board.name))
}

/// The most recent post's time, or `0` for an empty board — the feed's
/// `updated`/`lastBuildDate`.
fn latest(messages: &[crate::db::models::Message]) -> i64 {
    messages.iter().map(|m| m.created_at).max().unwrap_or(0)
}

fn render_atom(base: &str, board: &Board, messages: &[crate::db::models::Message]) -> String {
    let feed_url = format!("{base}/feed/{}", encode_path(&board.name));
    let title = if board.description.trim().is_empty() {
        board.name.clone()
    } else {
        format!("{} — {}", board.name, board.description)
    };
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str(&format!("  <title>{}</title>\n", xml_escape(&title)));
    out.push_str(&format!("  <id>{}</id>\n", xml_escape(&feed_url)));
    out.push_str(&format!(
        "  <link rel=\"self\" type=\"{ATOM_CONTENT_TYPE}\" href=\"{}\"/>\n",
        xml_escape(&feed_url)
    ));
    out.push_str(&format!(
        "  <link rel=\"alternate\" type=\"text/html\" href=\"{}/\"/>\n",
        xml_escape(base)
    ));
    out.push_str(&format!(
        "  <updated>{}</updated>\n",
        fmt_rfc3339(latest(messages))
    ));
    for m in messages {
        out.push_str("  <entry>\n");
        out.push_str(&format!("    <title>{}</title>\n", xml_escape(&m.subject)));
        out.push_str(&format!(
            "    <id>{}</id>\n",
            xml_escape(&entry_id(base, board, m.id))
        ));
        out.push_str(&format!(
            "    <link rel=\"alternate\" type=\"text/html\" href=\"{}/\"/>\n",
            xml_escape(base)
        ));
        out.push_str(&format!(
            "    <published>{}</published>\n",
            fmt_rfc3339(m.created_at)
        ));
        out.push_str(&format!(
            "    <updated>{}</updated>\n",
            fmt_rfc3339(m.edited_at.unwrap_or(m.created_at))
        ));
        out.push_str("    <author><name>");
        out.push_str(&xml_escape(&m.author_name));
        out.push_str("</name></author>\n");
        out.push_str(&format!(
            "    <content type=\"text\">{}</content>\n",
            xml_escape(&m.body)
        ));
        out.push_str("  </entry>\n");
    }
    out.push_str("</feed>\n");
    out
}

fn render_rss(base: &str, board: &Board, messages: &[crate::db::models::Message]) -> String {
    let feed_url = format!("{base}/feed/{}?format=rss", encode_path(&board.name));
    let description = if board.description.trim().is_empty() {
        format!("Posts on {}", board.name)
    } else {
        board.description.clone()
    };
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str("  <channel>\n");
    out.push_str(&format!("    <title>{}</title>\n", xml_escape(&board.name)));
    out.push_str(&format!("    <link>{}/</link>\n", xml_escape(base)));
    out.push_str(&format!(
        "    <description>{}</description>\n",
        xml_escape(&description)
    ));
    out.push_str(&format!(
        "    <atom:link href=\"{}\" rel=\"self\" type=\"{RSS_CONTENT_TYPE}\"/>\n",
        xml_escape(&feed_url)
    ));
    out.push_str(&format!(
        "    <lastBuildDate>{}</lastBuildDate>\n",
        fmt_rfc822(latest(messages))
    ));
    for m in messages {
        let id = entry_id(base, board, m.id);
        out.push_str("    <item>\n");
        out.push_str(&format!(
            "      <title>{}</title>\n",
            xml_escape(&m.subject)
        ));
        out.push_str(&format!(
            "      <description>{}</description>\n",
            xml_escape(&m.body)
        ));
        out.push_str(&format!(
            "      <author>{}</author>\n",
            xml_escape(&m.author_name)
        ));
        out.push_str(&format!(
            "      <pubDate>{}</pubDate>\n",
            fmt_rfc822(m.created_at)
        ));
        out.push_str(&format!(
            "      <guid isPermaLink=\"false\">{}</guid>\n",
            xml_escape(&id)
        ));
        out.push_str("    </item>\n");
    }
    out.push_str("  </channel>\n</rss>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{Board, Message};

    fn board() -> Board {
        Board {
            id: 3,
            name: "Off Topic".into(),
            description: "Anything goes & then some".into(),
            min_read_role: "guest".into(),
            min_write_role: "user".into(),
            locked: false,
        }
    }

    fn msg(id: i64, subject: &str, body: &str, created: i64) -> Message {
        Message {
            id,
            board_id: 3,
            author_id: 1,
            author_name: "alice".into(),
            subject: subject.into(),
            body: body.into(),
            created_at: created,
            pinned: false,
            parent_id: None,
            edited_at: None,
        }
    }

    #[test]
    fn escapes_all_five_entities_and_strips_control_chars() {
        assert_eq!(
            xml_escape("a & b < c > \"d\" 'e'"),
            "a &amp; b &lt; c &gt; &quot;d&quot; &apos;e&apos;"
        );
        // A stray control char (0x07 bell) is dropped; tab/newline survive.
        assert_eq!(xml_escape("x\u{07}y\tz\n"), "xy\tz\n");
    }

    #[test]
    fn path_encoding_is_conservative() {
        assert_eq!(encode_path("Off Topic"), "Off%20Topic");
        assert_eq!(encode_path("a&b/c"), "a%26b%2Fc");
        assert_eq!(encode_path("plain-name.1_~"), "plain-name.1_~");
    }

    #[test]
    fn atom_is_well_formed_and_escapes_hostile_content() {
        let msgs = [
            msg(2, "Second <post>", "body with & ampersand", 1_600_000_100),
            msg(1, "First", "hello", 1_600_000_000),
        ];
        let atom = render_atom("https://bbs.example.com", &board(), &msgs);

        assert!(atom.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
        assert!(atom.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
        // Feed self-link carries the percent-encoded board name.
        assert!(atom.contains("href=\"https://bbs.example.com/feed/Off%20Topic\""));
        // The hostile subject is escaped, never emitted raw.
        assert!(atom.contains("<title>Second &lt;post&gt;</title>"));
        assert!(!atom.contains("<post>"));
        assert!(atom.contains("body with &amp; ampersand"));
        // updated == newest post time.
        assert!(atom.contains(&format!(
            "<updated>{}</updated>",
            fmt_rfc3339(1_600_000_100)
        )));
        // Stable, unique entry ids.
        assert!(atom.contains("/feed/Off%20Topic#post-2"));
        assert!(atom.contains("/feed/Off%20Topic#post-1"));
        assert!(atom.trim_end().ends_with("</feed>"));
    }

    #[test]
    fn rss_is_well_formed() {
        let msgs = [msg(1, "Hi", "there", 1_600_000_000)];
        let rss = render_rss("https://bbs.example.com", &board(), &msgs);
        assert!(rss.contains("<rss version=\"2.0\""));
        assert!(rss.contains("<title>Off Topic</title>"));
        assert!(rss.contains("<description>Anything goes &amp; then some</description>"));
        assert!(rss.contains("<pubDate>"));
        assert!(rss.contains("isPermaLink=\"false\""));
        assert!(rss.contains(
            "<guid isPermaLink=\"false\">https://bbs.example.com/feed/Off%20Topic#post-1</guid>"
        ));
        assert!(rss.trim_end().ends_with("</rss>"));
    }

    #[test]
    fn an_empty_board_still_renders_a_valid_feed() {
        let atom = render_atom("https://bbs.example.com", &board(), &[]);
        assert!(atom.contains("<feed"));
        assert!(atom.contains("<updated>1970-01-01T00:00:00Z</updated>"));
        assert!(!atom.contains("<entry>"));
    }
}
