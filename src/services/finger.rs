//! A read-only `finger` service (RFC 1288, #77).
//!
//! Formats data we already hold — profile fields, last-on from the login trail,
//! post counts, and live who's-online — into the plain-text a `finger` client
//! expects. No auth and no writes: it only exposes what the in-BBS profile and
//! who's-online screens already show, minus anyone who has opted out.

use sqlx::sqlite::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::AppError;
use crate::services::{presence::Presence, profiles};
use crate::util::{fmt_time, now_unix};

/// Line terminator finger uses on the wire.
const CRLF: &str = "\r\n";

/// The most a client may send before we stop reading — a finger request is one
/// short line; anything longer is malformed or hostile.
const MAX_REQUEST: usize = 512;

/// Accept finger connections forever, one short request-response each. Errors on
/// a single connection are logged and dropped; the listener keeps running.
pub async fn serve(listener: TcpListener, pool: SqlitePool, presence: Presence) {
    loop {
        let (mut sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("finger accept failed: {e}");
                continue;
            }
        };
        let pool = pool.clone();
        let presence = presence.clone();
        tokio::spawn(async move {
            // Read a single line (up to the first CRLF or MAX_REQUEST bytes).
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match sock.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' {
                            break;
                        }
                        if byte[0] != b'\r' {
                            buf.push(byte[0]);
                        }
                        if buf.len() >= MAX_REQUEST {
                            break;
                        }
                    }
                    Err(_) => return,
                }
            }
            let request = String::from_utf8_lossy(&buf);
            let response = respond(&pool, &presence, &request).await;
            if let Err(e) = sock.write_all(response.as_bytes()).await {
                tracing::debug!("finger write to {peer} failed: {e}");
            }
            let _ = sock.shutdown().await;
        });
    }
}

/// Build the response for one finger request. `request` is the raw line the
/// client sent (without its trailing CRLF). An empty request lists who's online;
/// a bare username shows that user. Never errors — a lookup problem becomes a
/// message in the response, since finger has no other error channel.
pub async fn respond(pool: &SqlitePool, presence: &Presence, request: &str) -> String {
    // RFC 1288: an optional `/W` verbose switch may lead the line; we render the
    // same either way, so just strip it. Some clients also send `user@host`
    // rather than the bare local part — take what's before the first `@`.
    let query = request
        .trim()
        .trim_start_matches("/W")
        .trim()
        .split('@')
        .next()
        .unwrap_or("")
        .trim();

    if query.is_empty() {
        who_online(pool, presence).await
    } else {
        user(pool, presence, query).await
    }
}

/// The who's-online listing (`finger @host`), excluding opted-out users.
async fn who_online(pool: &SqlitePool, presence: &Presence) -> String {
    let online = presence.list().await;
    let hidden = optout_names(pool).await;
    let now = now_unix();

    let mut out = String::new();
    out.push_str("Who's online:");
    out.push_str(CRLF);
    let mut shown = 0;
    for u in &online {
        if hidden.contains(&u.username) {
            continue;
        }
        out.push_str(&format!(
            "  {:<20} on for {}{CRLF}",
            u.username,
            humanize_since(now - u.since)
        ));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("  (nobody)");
        out.push_str(CRLF);
    }
    out
}

/// A single user's finger card (`finger user@host`).
async fn user(pool: &SqlitePool, presence: &Presence, username: &str) -> String {
    // Remote actors live in `users` too but aren't members here; a finger lookup
    // should never surface one. A missing account, a remote actor, and an opted
    // -out user all read as the same "no such user" so the response can't be
    // used to probe who exists. `visible` is Some(true) only for a listed local
    // account.
    let visible: Option<bool> = sqlx::query_scalar::<_, i64>(
        "SELECT finger_optout FROM users WHERE username = ? AND is_remote = 0",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|optout| optout == 0);
    if visible != Some(true) {
        return no_such_user(username);
    }

    let profile = match profiles::get_profile_by_name(pool, username).await {
        Ok(p) => p,
        Err(AppError::NotFound) => return no_such_user(username),
        Err(e) => return format!("finger: lookup failed: {e}{CRLF}"),
    };

    let dash = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    let online = presence
        .list()
        .await
        .into_iter()
        .find(|u| u.username == username);

    let mut out = String::new();
    out.push_str(&format!(
        "Login: {:<20} Name: {}{CRLF}",
        profile.username,
        dash(&profile.real_name)
    ));
    out.push_str(&format!(
        "Location: {:<17} Tagline: {}{CRLF}",
        dash(&profile.location),
        dash(&profile.tagline)
    ));
    out.push_str(&format!(
        "Member since: {}{CRLF}",
        fmt_time(profile.created_at)
    ));
    out.push_str(&format!(
        "Last on: {}{CRLF}",
        profile
            .last_login
            .map(fmt_time)
            .unwrap_or_else(|| "never".to_string())
    ));
    out.push_str(&format!("Posts: {}{CRLF}", profile.post_count));
    match online {
        Some(u) => out.push_str(&format!(
            "Status: online (on for {}){CRLF}",
            humanize_since(now_unix() - u.since)
        )),
        None => out.push_str(&format!("Status: offline{CRLF}")),
    }
    if !profile.signature.is_empty() {
        out.push_str(CRLF);
        out.push_str(&format!("-- {}{CRLF}", profile.signature));
    }
    out
}

fn no_such_user(username: &str) -> String {
    format!("finger: no such user \"{username}\".{CRLF}")
}

/// The set of usernames that have opted out of finger.
async fn optout_names(pool: &SqlitePool) -> std::collections::HashSet<String> {
    sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE finger_optout = 1")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// A coarse "1h 5m"-style duration for a span given in seconds.
fn humanize_since(secs: i64) -> String {
    let secs = secs.max(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        "just now".to_string()
    }
}
