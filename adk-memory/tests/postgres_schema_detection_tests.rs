//! Baseline detection of `PostgresMemoryService::migrate` against a live server.
//!
//! `migrate` records the baseline migration as applied when `memory_entries` already
//! exists. Only a table in the connection's current schema counts; a `memory_entries`
//! table in another schema of the same database is a different deployment.
//!
//! The assertions hold with or without the pgvector extension: without it, `migrate`
//! fails at `CREATE EXTENSION vector` and records nothing.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`); the test drops the schemas it creates:
//!
//! ```bash
//! DATABASE_URL=postgres://user@localhost/scratch \
//!   cargo nextest run -p adk-memory --features database-memory --run-ignored only postgres_schema
//! ```

#![cfg(feature = "database-memory")]

use adk_memory::PostgresMemoryService;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use std::str::FromStr;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_schema_migrate_ignores_tables_in_other_schemas() {
    let mut admin = PgConnection::connect(&database_url()).await.expect("connect admin");
    let run = uuid::Uuid::new_v4().simple().to_string();
    let (other, target) = (format!("adk_other_{run}"), format!("adk_target_{run}"));
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {other}; CREATE TABLE {other}.memory_entries (id INT); \
         CREATE SCHEMA {target};"
    ))
    .execute(&mut admin)
    .await
    .expect("create schemas");

    let options = PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .options([("search_path", target.as_str())]);
    let pool =
        PgPoolOptions::new().max_connections(2).connect_with(options).await.expect("connect");
    let service = PostgresMemoryService::from_pool(pool, None);

    let migrated = service.migrate().await;
    let version = service.schema_version().await;
    let table_in_target: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = 'memory_entries')",
    )
    .bind(&target)
    .fetch_one(&mut admin)
    .await
    .expect("query information_schema");

    sqlx::raw_sql(&format!("DROP SCHEMA {other} CASCADE; DROP SCHEMA {target} CASCADE;"))
        .execute(&mut admin)
        .await
        .expect("drop schemas");

    // Either the migration ran in the target schema, or it failed before recording
    // anything; it never takes the other schema's table as its baseline.
    match migrated {
        Ok(()) => assert_eq!((version.ok(), table_in_target), (Some(2), true)),
        Err(error) => assert_eq!(
            (version.ok(), table_in_target),
            (Some(0), false),
            "migrate failed after recording a baseline: {error}"
        ),
    }
}
