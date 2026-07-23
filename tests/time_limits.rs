//! Per-user daily time limits (#75): banked usage plus live session time, with
//! a one-shot warning near the cap and a Quit at it. Admins are exempt.

use std::collections::HashSet;

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::mpsc;

use bbs_rs::config::Limits;
use bbs_rs::db::models::User;
use bbs_rs::services::presence::Presence;
use bbs_rs::services::{admin, auth, timelimit};
use bbs_rs::ssh::server::enforce_time_limits;
use bbs_rs::transport::Event;

async fn setup() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();
    pool
}

async fn reg(pool: &SqlitePool, name: &str) -> User {
    auth::register_user(pool, name, "pw", &Default::default())
        .await
        .unwrap()
}

fn limits(daily_minutes: u32) -> Limits {
    Limits {
        daily_minutes,
        ..Default::default()
    }
}

/// A presence registry with one live session for `name`, plus its receiver.
async fn one_session(name: &str) -> (Presence, mpsc::Receiver<Event>) {
    let presence = Presence::new();
    let (tx, rx) = mpsc::channel(16);
    presence.join(1, name.to_string(), None, tx).await;
    (presence, rx)
}

#[tokio::test]
async fn usage_accumulates_per_user_per_day() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let day = timelimit::day_key(1_000_000);

    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, day).await.unwrap(),
        0
    );
    timelimit::add_seconds(&pool, alice.id, day, 120)
        .await
        .unwrap();
    timelimit::add_seconds(&pool, alice.id, day, 60)
        .await
        .unwrap();
    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, day).await.unwrap(),
        180,
        "banked time adds up"
    );
    // A different day is a separate budget.
    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, day + 1)
            .await
            .unwrap(),
        0
    );
    // Zero/negative durations are ignored (clock skew can't refund time).
    timelimit::add_seconds(&pool, alice.id, day, -500)
        .await
        .unwrap();
    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, day).await.unwrap(),
        180
    );
}

#[tokio::test]
async fn purge_drops_old_days_only() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    timelimit::add_seconds(&pool, alice.id, 100, 60)
        .await
        .unwrap();
    timelimit::add_seconds(&pool, alice.id, 200, 60)
        .await
        .unwrap();

    assert_eq!(timelimit::purge_before(&pool, 150).await.unwrap(), 1);
    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, 100).await.unwrap(),
        0
    );
    assert_eq!(
        timelimit::seconds_used(&pool, alice.id, 200).await.unwrap(),
        60
    );
}

#[tokio::test]
async fn well_under_the_cap_nothing_happens() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let today = timelimit::day_key(bbs_rs::util::now_unix());
    timelimit::add_seconds(&pool, alice.id, today, 60)
        .await
        .unwrap();
    let (presence, mut rx) = one_session("alice").await;

    let mut warned = HashSet::new();
    enforce_time_limits(&pool, &presence, &limits(60), &mut warned).await;
    assert!(rx.try_recv().is_err(), "no notice, no kick");
}

#[tokio::test]
async fn near_the_cap_warns_once() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let today = timelimit::day_key(bbs_rs::util::now_unix());
    // 10-minute cap, 400s banked → inside the 5-minute warning window.
    timelimit::add_seconds(&pool, alice.id, today, 400)
        .await
        .unwrap();
    let (presence, mut rx) = one_session("alice").await;

    let mut warned = HashSet::new();
    enforce_time_limits(&pool, &presence, &limits(10), &mut warned).await;
    match rx.try_recv() {
        Ok(Event::Notice { text }) => assert!(text.contains("daily time"), "got: {text}"),
        other => panic!("expected a time warning, got {other:?}"),
    }

    // A second sweep doesn't nag again.
    enforce_time_limits(&pool, &presence, &limits(10), &mut warned).await;
    assert!(rx.try_recv().is_err(), "warned only once");
}

#[tokio::test]
async fn at_the_cap_the_session_is_ended() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let today = timelimit::day_key(bbs_rs::util::now_unix());
    timelimit::add_seconds(&pool, alice.id, today, 601)
        .await
        .unwrap();
    let (presence, mut rx) = one_session("alice").await;

    let mut warned = HashSet::new();
    enforce_time_limits(&pool, &presence, &limits(10), &mut warned).await;
    assert!(
        matches!(rx.try_recv(), Ok(Event::Quit)),
        "over the cap ends the session"
    );
}

#[tokio::test]
async fn admins_are_exempt() {
    let pool = setup().await;
    reg(&pool, "boss").await;
    admin::set_role(&pool, "boss", "admin").await.unwrap();
    let boss = auth::find_user(&pool, "boss").await.unwrap().unwrap();
    let today = timelimit::day_key(bbs_rs::util::now_unix());
    timelimit::add_seconds(&pool, boss.id, today, 99_999)
        .await
        .unwrap();
    let (presence, mut rx) = one_session("boss").await;

    let mut warned = HashSet::new();
    enforce_time_limits(&pool, &presence, &limits(10), &mut warned).await;
    assert!(rx.try_recv().is_err(), "an admin is never cut off");
}

#[tokio::test]
async fn a_zero_cap_disables_the_limit() {
    let pool = setup().await;
    let alice = reg(&pool, "alice").await;
    let today = timelimit::day_key(bbs_rs::util::now_unix());
    timelimit::add_seconds(&pool, alice.id, today, 99_999)
        .await
        .unwrap();
    let (presence, mut rx) = one_session("alice").await;

    let mut warned = HashSet::new();
    enforce_time_limits(&pool, &presence, &limits(0), &mut warned).await;
    assert!(rx.try_recv().is_err(), "0 = unlimited");
}
