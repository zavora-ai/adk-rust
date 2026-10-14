//! Databases written by adk-memory 2.3.0, the last SQLx 0.8 release, open with the
//! current crate: entries read back, the migration registry continues without a
//! repeated or failed step, and new entries are searchable.
//!
//! The fixtures are in `tests/fixtures/sqlx-0.8/`. The SQLite test runs by default; the
//! PostgreSQL test creates and drops a database on the server at `DATABASE_URL`, which
//! needs the pgvector extension:
//!
//! ```bash
//! docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-memory --features sqlite-memory,database-memory \
//!   --run-ignored all --test upgrade_from_sqlx_0_8
//! ```

#![cfg(any(feature = "sqlite-memory", feature = "database-memory"))]

use adk_core::{Content, Part};
use adk_memory::{MemoryEntry, MemoryService, SearchRequest};
use chrono::Utc;

const APP: &str = "upgrade_app";
const USER: &str = "upgrade_user";

async fn search(service: &dyn MemoryService, query: &str) -> Vec<String> {
    service
        .search(SearchRequest {
            query: query.to_string(),
            user_id: USER.to_string(),
            app_name: APP.to_string(),
            limit: Some(10),
            min_score: None,
            project_id: None,
        })
        .await
        .expect("search")
        .memories
        .iter()
        .flat_map(|memory| &memory.content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Finds the 2.3.0 entry, adds one, and deletes the 2.3.0 session.
async fn assert_upgraded(service: &dyn MemoryService) {
    assert_eq!(search(service, "harbour").await, ["O'Brien prefers the harbour at high tide"]);

    let entry = MemoryEntry {
        content: Content::new("user").with_text("The harbour master logs every tide"),
        author: "user".to_string(),
        timestamp: Utc::now(),
    };
    service.add_session(APP, USER, "after_upgrade", vec![entry]).await.expect("add entry");
    assert_eq!(search(service, "harbour").await.len(), 2);

    service.delete_session(APP, USER, "upgrade_session").await.expect("delete session");
    assert_eq!(search(service, "harbour").await, ["The harbour master logs every tide"]);
}

#[cfg(feature = "sqlite-memory")]
#[tokio::test]
async fn a_2_3_sqlite_database_upgrades() {
    use adk_memory::SqliteMemoryService;
    use sqlx::{Connection, SqliteConnection};

    let path = std::env::temp_dir().join(format!("adk-memory-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let mut conn = SqliteConnection::connect(&url).await.expect("create database");
    sqlx::raw_sql(include_str!("fixtures/sqlx-0.8/memory_sqlite.sql"))
        .execute(&mut conn)
        .await
        .expect("load the 2.3.0 fixture");
    conn.close().await.expect("close");

    let service = SqliteMemoryService::new(&url).await.expect("open");
    service.migrate().await.expect("migrate");
    service.migrate().await.expect("migrate is idempotent");
    assert_eq!(service.schema_version().await.expect("schema version"), 2);
    assert_upgraded(&service).await;

    let mut conn = SqliteConnection::connect(&url).await.expect("connect");
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_memory_migrations ORDER BY version")
            .fetch_all(&mut conn)
            .await
            .expect("read registry");
    assert_eq!(registry, [1, 2]);
    conn.close().await.expect("close");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "database-memory")]
#[tokio::test]
#[ignore = "requires a PostgreSQL server with the pgvector extension at DATABASE_URL"]
async fn a_2_3_postgres_database_upgrades() {
    use adk_memory::PostgresMemoryService;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::{Connection, PgConnection};
    use std::str::FromStr;

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    let database = format!("adk_memory_upgrade_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&url).await.expect("connect admin");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&mut admin)
        .await
        .expect("create database");
    let options = PgConnectOptions::from_str(&url).expect("parse DATABASE_URL").database(&database);

    // The fixture holds the tables only; 2.3.0 created the extension in `public`.
    let mut conn = PgConnection::connect_with(&options).await.expect("connect");
    sqlx::query("CREATE EXTENSION vector SCHEMA public")
        .execute(&mut conn)
        .await
        .expect("pgvector is available");
    sqlx::raw_sql(include_str!("fixtures/sqlx-0.8/memory_postgres.sql"))
        .execute(&mut conn)
        .await
        .expect("load the 2.3.0 fixture");
    conn.close().await.expect("close");

    let pool = PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("pool");
    let service = PostgresMemoryService::from_pool(pool.clone(), None);
    service.migrate().await.expect("migrate");
    service.migrate().await.expect("migrate is idempotent");
    assert_eq!(service.schema_version().await.expect("schema version"), 2);
    assert_upgraded(&service).await;
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_memory_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("read registry");
    assert_eq!(registry, [1, 2]);

    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {database} WITH (FORCE)")))
        .execute(&mut admin)
        .await
        .expect("drop database");
}
