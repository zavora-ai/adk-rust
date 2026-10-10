//! Advisory-lock handling of `PostgresSessionService::migrate` against a live server.
//!
//! `migrate` serialises concurrent instances with a session-level advisory lock. The
//! lock belongs to one server connection, so it must be taken and released on the same
//! pooled connection; otherwise an idle pooled connection keeps it and a second
//! instance blocks forever in `migrate`.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`); the test creates the session tables
//! and briefly inserts a registry row:
//!
//! ```bash
//! DATABASE_URL=postgres://user@localhost/scratch \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only postgres_migrate
//! ```

#![cfg(feature = "postgres")]

use adk_session::PostgresSessionService;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use std::str::FromStr;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

/// `application_name` of every connection this test opens, so lock counts exclude the
/// migration lock that other tests running `migrate` against the same database hold.
const APPLICATION_NAME: &str = "adk-session-migrate-lock-test";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

fn connect_options() -> PgConnectOptions {
    PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .application_name(APPLICATION_NAME)
}

async fn pool(max_connections: u32) -> PgPool {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_with(connect_options())
        .await
        .expect("connect to postgres")
}

/// Grants of the migration advisory lock in the current database held by this test's
/// connections, as seen from outside any pool. Other advisory locks, such as another
/// crate's migration lock, and other processes' connections are not counted.
async fn advisory_locks_held(observer: &mut PgConnection) -> i64 {
    // `pg_locks` shows a bigint key as its high and low 32 bits.
    let key = PostgresSessionService::ADVISORY_LOCK_KEY as u64;
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid \
         WHERE l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
           AND l.classid::bigint = $1 AND l.objid::bigint = $2 \
           AND l.database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
           AND a.application_name = $3",
    )
    .bind((key >> 32) as i64)
    .bind((key & 0xffff_ffff) as i64)
    .bind(APPLICATION_NAME)
    .fetch_one(observer)
    .await
    .expect("query pg_locks")
}

async fn migrate(service: &PostgresSessionService, label: &str) -> adk_core::Result<()> {
    tokio::time::timeout(TIMEOUT, service.migrate())
        .await
        .unwrap_or_else(|_| panic!("{label}: migrate() still blocked after {TIMEOUT:?}"))
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_migrate_releases_advisory_lock_across_instances() {
    let mut observer =
        PgConnection::connect_with(&connect_options()).await.expect("connect observer");

    // The key counted above is the one `migrate` waits on.
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(PostgresSessionService::ADVISORY_LOCK_KEY)
        .execute(&mut observer)
        .await
        .expect("take the migration lock");
    assert_eq!(advisory_locks_held(&mut observer).await, 1);
    let blocked = PostgresSessionService::from_pool(pool(4).await);
    let mut waiting = Box::pin(blocked.migrate());
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut waiting).await.is_err(),
        "migrate() ran while another connection held ADVISORY_LOCK_KEY"
    );
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(PostgresSessionService::ADVISORY_LOCK_KEY)
        .execute(&mut observer)
        .await
        .expect("release the migration lock");
    tokio::time::timeout(TIMEOUT, waiting)
        .await
        .expect("migrate() still blocked after the lock was released")
        .expect("migrate after the lock was released");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);

    // Two pools model two replicas of one application.
    let first = PostgresSessionService::from_pool(pool(4).await);
    let second = PostgresSessionService::from_pool(pool(4).await);

    migrate(&first, "first instance").await.expect("first migrate");
    assert_eq!(advisory_locks_held(&mut observer).await, 0, "lock left held after migrate");

    migrate(&second, "second instance").await.expect("second migrate");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);

    let (a, b) =
        tokio::time::timeout(TIMEOUT, async { tokio::join!(first.migrate(), second.migrate()) })
            .await
            .expect("concurrent migrate() calls still blocked");
    a.expect("concurrent migrate, first instance");
    b.expect("concurrent migrate, second instance");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);

    // The lock connection runs every migration statement, so one connection is enough.
    let single = PostgresSessionService::from_pool(pool(1).await);
    migrate(&single, "single-connection pool").await.expect("single-connection migrate");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);

    // A failed migration still releases the lock.
    sqlx::query(
        "INSERT INTO _adk_session_migrations (version, description, applied_at) \
         VALUES (999999, 'lock test', 'now')",
    )
    .execute(&mut observer)
    .await
    .expect("insert future registry version");
    let failed = migrate(&first, "migrate against a newer schema").await;
    sqlx::query("DELETE FROM _adk_session_migrations WHERE version = 999999")
        .execute(&mut observer)
        .await
        .expect("remove future registry version");
    assert!(failed.is_err(), "migrate accepted a schema newer than the code");
    assert_eq!(advisory_locks_held(&mut observer).await, 0, "lock left held after a failure");

    migrate(&second, "after a failed migrate").await.expect("migrate after failure");
    assert_eq!(advisory_locks_held(&mut observer).await, 0);
}
