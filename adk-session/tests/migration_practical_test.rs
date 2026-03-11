//! Practical end-to-end tests for the schema migration runner.
//!
//! These tests exercise the full migration lifecycle against in-memory SQLite:
//! fresh database, idempotence, schema_version, baseline detection, and CRUD.

#![cfg(feature = "sqlite")]

use adk_session::SqliteSessionService;
use adk_session::service::{CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService};
use sqlx::{Row, SqlitePool};

/// Fresh database: migrate() creates all tables and registry, schema_version returns 1.
#[tokio::test]
async fn fresh_db_migrate_creates_tables_and_registry() {
    let svc = SqliteSessionService::new("sqlite::memory:").await.unwrap();

    // Before migrate: schema_version should be 0
    let v = svc.schema_version().await.unwrap();
    assert_eq!(v, 0, "schema_version should be 0 before migrate");

    // Run migrate
    svc.migrate().await.unwrap();

    // After migrate: schema_version should be 1
    let v = svc.schema_version().await.unwrap();
    assert_eq!(v, 1, "schema_version should be 1 after migrate");
}

/// Idempotence: calling migrate() twice is a no-op the second time.
#[tokio::test]
async fn migrate_twice_is_idempotent() {
    let svc = SqliteSessionService::new("sqlite::memory:").await.unwrap();

    svc.migrate().await.unwrap();
    let v1 = svc.schema_version().await.unwrap();

    // Second call should succeed without errors
    svc.migrate().await.unwrap();
    let v2 = svc.schema_version().await.unwrap();

    assert_eq!(v1, v2, "schema_version should not change on second migrate");
    assert_eq!(v2, 1);
}

/// After migration, full CRUD works: create session, get, list, delete.
#[tokio::test]
async fn crud_works_after_migration() {
    let svc = SqliteSessionService::new("sqlite::memory:").await.unwrap();
    svc.migrate().await.unwrap();

    // Create
    let session = svc
        .create(CreateRequest {
            app_name: "myapp".into(),
            user_id: "user1".into(),
            session_id: Some("sess-1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(session.id(), "sess-1");

    // Get
    let fetched = svc
        .get(GetRequest {
            app_name: "myapp".into(),
            user_id: "user1".into(),
            session_id: "sess-1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    assert_eq!(fetched.id(), "sess-1");

    // List
    let list = svc
        .list(ListRequest {
            app_name: "myapp".into(),
            user_id: "user1".into(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    assert_eq!(list.len(), 1);

    // Delete
    svc.delete(DeleteRequest {
        app_name: "myapp".into(),
        user_id: "user1".into(),
        session_id: "sess-1".into(),
    })
    .await
    .unwrap();
    let after_delete = svc
        .get(GetRequest {
            app_name: "myapp".into(),
            user_id: "user1".into(),
            session_id: "sess-1".into(),
            num_recent_events: None,
            after: None,
        })
        .await;
    assert!(after_delete.is_err(), "get after delete should fail");
}

/// Baseline detection: if tables already exist (old-style CREATE IF NOT EXISTS),
/// migrate() should detect them and record v1 without re-creating.
#[tokio::test]
async fn baseline_detection_on_preexisting_tables() {
    // Create a pool and manually create the sessions table (simulating old code)
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE sessions (\
            app_name TEXT NOT NULL, \
            user_id TEXT NOT NULL, \
            session_id TEXT NOT NULL, \
            state TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL, \
            PRIMARY KEY (app_name, user_id, session_id)\
        )",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert a row to prove the table is real
    sqlx::query(
        "INSERT INTO sessions (app_name, user_id, session_id, state, created_at, updated_at) \
         VALUES ('app', 'u1', 's1', '{}', '2025-01-01', '2025-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Now create the service from this pool and migrate
    let svc = SqliteSessionService::from_pool(pool);
    svc.migrate().await.unwrap();

    // Should detect baseline and record v1
    let v = svc.schema_version().await.unwrap();
    assert_eq!(v, 1, "baseline should be recorded as v1");

    // The pre-existing row should still be there
    let row =
        sqlx::query("SELECT COUNT(*) AS cnt FROM sessions").fetch_one(svc.pool()).await.unwrap();
    let count: i64 = row.try_get("cnt").unwrap();
    assert_eq!(count, 1, "pre-existing data should be preserved");
}

/// Registry records: verify the migration registry has the expected content.
#[tokio::test]
async fn registry_records_are_complete() {
    let svc = SqliteSessionService::new("sqlite::memory:").await.unwrap();
    svc.migrate().await.unwrap();

    let row = sqlx::query(
        "SELECT version, description, applied_at FROM _adk_session_migrations WHERE version = 1",
    )
    .fetch_one(svc.pool())
    .await
    .unwrap();

    let version: i64 = row.try_get("version").unwrap();
    let description: String = row.try_get("description").unwrap();
    let applied_at: String = row.try_get("applied_at").unwrap();

    assert_eq!(version, 1);
    assert!(!description.is_empty(), "description should not be empty");
    assert!(!applied_at.is_empty(), "applied_at should not be empty");
}
