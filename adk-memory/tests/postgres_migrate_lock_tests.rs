//! Advisory-lock handling of `PostgresMemoryService::migrate` against a live server.
//!
//! `migrate` serialises concurrent instances with a session-level advisory lock. The
//! lock belongs to one server connection, so it must be taken and released on the same
//! pooled connection; otherwise an idle pooled connection keeps it and a second
//! instance blocks forever in `migrate`.
//!
//! The assertions hold with or without the pgvector extension: without it, every
//! `migrate` call fails at `CREATE EXTENSION vector`, which exercises the failure path.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`):
//!
//! ```bash
//! DATABASE_URL=postgres://user@localhost/scratch \
//!   cargo nextest run -p adk-memory --features database-memory --run-ignored only postgres_migrate
//! ```

#![cfg(feature = "database-memory")]

use adk_memory::PostgresMemoryService;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

async fn pool(max_connections: u32) -> PgPool {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&database_url())
        .await
        .expect("connect to postgres")
}

/// Advisory locks granted in the current database, as seen from outside any pool.
async fn advisory_locks_held(observer: &mut PgConnection) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks \
         WHERE locktype = 'advisory' AND granted \
           AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(observer)
    .await
    .expect("query pg_locks")
}

async fn migrate(service: &PostgresMemoryService, label: &str) -> adk_core::Result<()> {
    tokio::time::timeout(TIMEOUT, service.migrate())
        .await
        .unwrap_or_else(|_| panic!("{label}: migrate() still blocked after {TIMEOUT:?}"))
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_migrate_releases_advisory_lock_across_instances() {
    let mut observer = PgConnection::connect(&database_url()).await.expect("connect observer");

    // Two pools model two replicas of one application.
    let first = PostgresMemoryService::from_pool(pool(4).await, None);
    let second = PostgresMemoryService::from_pool(pool(4).await, None);

    let first_result = migrate(&first, "first instance").await;
    assert_eq!(advisory_locks_held(&mut observer).await, 0, "lock left held after migrate");

    let second_result = migrate(&second, "second instance").await;
    assert_eq!(advisory_locks_held(&mut observer).await, 0);
    assert_eq!(
        first_result.is_ok(),
        second_result.is_ok(),
        "instances disagree: {first_result:?} vs {second_result:?}"
    );

    let (a, b) =
        tokio::time::timeout(TIMEOUT, async { tokio::join!(first.migrate(), second.migrate()) })
            .await
            .expect("concurrent migrate() calls still blocked");
    assert_eq!((a.is_ok(), b.is_ok()), (first_result.is_ok(), first_result.is_ok()));
    assert_eq!(advisory_locks_held(&mut observer).await, 0);

    // The lock connection runs every migration statement, so one connection is enough.
    let single = PostgresMemoryService::from_pool(pool(1).await, None);
    let single_result = migrate(&single, "single-connection pool").await;
    assert_eq!(single_result.is_ok(), first_result.is_ok(), "{single_result:?}");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);
}
