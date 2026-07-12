//! Tests for the explicit migration status/reporting helpers.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

/// A fresh in-memory DB with **no** migrations applied yet.
async fn fresh_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite")
}

#[tokio::test]
async fn reporting_applies_all_then_is_idempotent() {
    let pool = fresh_pool().await;

    // First run applies every embedded migration…
    let newly = bbs_rs::db::run_migrations_reporting(&pool).await.unwrap();
    assert!(!newly.is_empty(), "a fresh DB should apply migrations");
    assert!(
        newly.windows(2).all(|w| w[0] < w[1]),
        "ascending version order"
    );

    // …a second run applies nothing.
    let again = bbs_rs::db::run_migrations_reporting(&pool).await.unwrap();
    assert!(again.is_empty(), "already-migrated DB reports nothing new");
}

#[tokio::test]
async fn status_reflects_applied_state() {
    let pool = fresh_pool().await;

    // Before migrating, every migration is pending.
    let before = bbs_rs::db::migration_status(&pool).await.unwrap();
    assert!(!before.is_empty());
    assert!(
        before.iter().all(|m| !m.applied),
        "all pending before migrate"
    );

    let newly = bbs_rs::db::run_migrations_reporting(&pool).await.unwrap();
    assert_eq!(newly.len(), before.len(), "first run applies all of them");

    // After migrating, every migration is applied.
    let after = bbs_rs::db::migration_status(&pool).await.unwrap();
    assert!(after.iter().all(|m| m.applied), "all applied after migrate");
}
