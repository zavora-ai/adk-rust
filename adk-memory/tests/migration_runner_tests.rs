//! The connection-based SQL migration runner against in-memory SQLite.

#![cfg(feature = "sqlite-memory")]

use adk_memory::migration::sqlite_runner;
use sqlx::{Connection, Row, SqliteConnection};

const STEPS: &[(i64, &str, &str)] = &[
    (1, "create widgets", "CREATE TABLE widgets (id INTEGER PRIMARY KEY)"),
    (2, "add widget name", "ALTER TABLE widgets ADD COLUMN name TEXT"),
];

async fn migrate(conn: &mut SqliteConnection) -> adk_core::Result<()> {
    sqlite_runner::run_sql_migrations_on_connection(conn, "_widget_migrations", STEPS, |conn| {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type='table' AND name='widgets'",
            )
            .fetch_one(conn)
            .await
            .map_err(|e| adk_core::AdkError::memory(e.to_string()))?;
            Ok(row.try_get::<i64, _>("cnt").unwrap_or(0) > 0)
        })
    })
    .await
}

async fn applied_versions(conn: &mut SqliteConnection) -> Vec<i64> {
    sqlx::query_scalar("SELECT version FROM _widget_migrations ORDER BY version")
        .fetch_all(conn)
        .await
        .unwrap()
}

/// An in-memory database is per connection, so every step ran on `conn`.
#[tokio::test]
async fn run_on_connection_applies_steps_once_and_records_baseline() {
    let mut fresh = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    migrate(&mut fresh).await.unwrap();
    migrate(&mut fresh).await.unwrap();
    assert_eq!(applied_versions(&mut fresh).await, vec![1, 2]);
    sqlx::query("INSERT INTO widgets (id, name) VALUES (1, 'a')")
        .execute(&mut fresh)
        .await
        .unwrap();

    let mut existing = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE widgets (id INTEGER PRIMARY KEY)")
        .execute(&mut existing)
        .await
        .unwrap();
    migrate(&mut existing).await.unwrap();
    assert_eq!(applied_versions(&mut existing).await, vec![1, 2]);
    sqlx::query("INSERT INTO widgets (id, name) VALUES (1, 'a')")
        .execute(&mut existing)
        .await
        .unwrap();
}
