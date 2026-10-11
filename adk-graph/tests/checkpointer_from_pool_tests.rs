//! `SqliteCheckpointer::from_pool` has to adopt a caller's pool, not open its own.
//!
//! An application that already owns a `SqlitePool` — for sessions, for its own
//! tables — should be able to hand that pool over and keep one connection pool
//! for the process. That only holds if the checkpointer writes through the pool
//! it was given, and if it still applies its schema to that pool's database.

#![cfg(feature = "sqlite")]

use adk_graph::checkpoint::{Checkpointer, SqliteCheckpointer};
use adk_graph::state::Checkpoint;
use serde_json::json;

fn checkpoint(thread: &str, id: &str, step: usize) -> Checkpoint {
    serde_json::from_value(json!({
        "thread_id": thread,
        "checkpoint_id": id,
        "state": { "value": step },
        "step": step,
        "pending_nodes": ["next"],
        "metadata": {},
        "created_at": "2026-08-20T00:00:00Z",
        "version": 1
    }))
    .expect("test checkpoint must build")
}

/// A checkpointer built from a caller's pool round-trips a checkpoint.
#[tokio::test]
async fn from_pool_adopts_the_callers_pool() {
    let pool =
        sqlx::SqlitePool::connect("sqlite::memory:").await.expect("caller opens its own pool");

    let cp = SqliteCheckpointer::from_pool(pool.clone())
        .await
        .expect("from_pool must accept an open pool");

    cp.save(&checkpoint("t-1", "cp-1", 1)).await.expect("save through the adopted pool");

    let loaded = cp.load("t-1").await.expect("load must succeed");
    assert_eq!(
        loaded.map(|c| (c.checkpoint_id, c.step)),
        Some(("cp-1".to_string(), 1)),
        "a checkpoint saved through the adopted pool must load back"
    );
}

/// The caller's own handle sees the checkpointer's writes.
///
/// This is the whole point of the API: one pool, one database. If `from_pool`
/// opened its own connection instead, this query would find nothing — an
/// in-memory SQLite database is private to its pool.
#[tokio::test]
async fn the_caller_can_query_what_the_checkpointer_wrote() {
    let pool =
        sqlx::SqlitePool::connect("sqlite::memory:").await.expect("caller opens its own pool");

    let cp = SqliteCheckpointer::from_pool(pool.clone())
        .await
        .expect("from_pool must accept an open pool");

    cp.save(&checkpoint("t-shared", "cp-shared", 4)).await.expect("save");

    // Queried through the caller's handle, not the checkpointer's.
    let (id, step): (String, i64) =
        sqlx::query_as("SELECT id, step FROM graph_checkpoints WHERE thread_id = ?")
            .bind("t-shared")
            .fetch_one(&pool)
            .await
            .expect("the caller's pool must see the checkpointer's row");

    assert_eq!(
        (id.as_str(), step),
        ("cp-shared", 4),
        "the checkpointer must write through the pool it was given"
    );
}

/// `from_pool` applies the schema, including the columns added after 1.0.
///
/// `new` and `from_pool` share one body, so a caller-supplied pool must get the
/// same table, the same late-added columns, and the same index.
#[tokio::test]
async fn from_pool_creates_the_full_schema() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.expect("pool");

    SqliteCheckpointer::from_pool(pool.clone()).await.expect("from_pool");

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('graph_checkpoints') ORDER BY name")
            .fetch_all(&pool)
            .await
            .expect("the table must exist in the caller's database");

    for required in [
        "attempts",
        "child_ledger",
        "cleared_interrupt",
        "created_at",
        "id",
        "metadata",
        "pending_nodes",
        "state",
        "step",
        "thread_id",
        "version",
        "pending_writes",
    ] {
        assert!(
            columns.iter().any(|c| c == required),
            "column `{required}` missing from a from_pool database; found {columns:?}"
        );
    }

    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(&pool)
            .await
            .expect("index query");

    assert!(
        indexes.iter().any(|i| i == "idx_graph_checkpoints_thread_version"),
        "the thread version index must be created on a caller-supplied pool; found {indexes:?}"
    );
}

/// Adopting a pool twice is safe.
///
/// Two checkpointers over one database is a plausible shape — a subgraph with its
/// own handle, say — and the schema statements are all `IF NOT EXISTS` or
/// deliberately ignored, so the second call must not fail.
#[tokio::test]
async fn adopting_the_same_pool_twice_succeeds() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.expect("pool");

    let first = SqliteCheckpointer::from_pool(pool.clone()).await;
    assert!(first.is_ok(), "first adoption: {:?}", first.err());

    let second = SqliteCheckpointer::from_pool(pool.clone()).await;
    assert!(
        second.is_ok(),
        "re-running the schema on an initialized database must not fail: {:?}",
        second.err()
    );

    // And the pair still share one database.
    first.unwrap().save(&checkpoint("t-twice", "cp-twice", 9)).await.expect("save via the first");

    let loaded = second.unwrap().load("t-twice").await.expect("load via the second");
    assert_eq!(
        loaded.map(|c| c.checkpoint_id),
        Some("cp-twice".to_string()),
        "both checkpointers must address the same database"
    );
}

/// A database written before checkpoints carried versions numbers its rows in
/// creation order, so the newest stays the newest.
#[tokio::test]
async fn a_database_without_versions_is_numbered_in_creation_order() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.expect("pool");
    sqlx::query(
        "CREATE TABLE graph_checkpoints (id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, \
         state TEXT NOT NULL, step INTEGER NOT NULL, pending_nodes TEXT NOT NULL, metadata TEXT, \
         created_at TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("legacy table");
    for (id, created_at) in [("late", "2026-08-20T00:00:02Z"), ("early", "2026-08-20T00:00:01Z")] {
        sqlx::query("INSERT INTO graph_checkpoints VALUES (?, 'legacy', '{}', 0, '[]', '{}', ?)")
            .bind(id)
            .bind(created_at)
            .execute(&pool)
            .await
            .expect("legacy row");
    }

    let cp = SqliteCheckpointer::from_pool(pool).await.expect("migrate");
    let listed: Vec<(String, u64)> = cp
        .list("legacy")
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.checkpoint_id, c.version))
        .collect();
    assert_eq!(listed, vec![("early".to_string(), 1), ("late".to_string(), 2)]);
    assert_eq!(cp.load("legacy").await.unwrap().map(|c| c.checkpoint_id), Some("late".to_string()));
    cp.save(&checkpoint("legacy", "next", 1)).await.expect_err("version 1 is behind");
}
