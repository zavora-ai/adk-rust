//! The PostgreSQL ledger store (spec 3.2 §7.1, §7.10, §11, §13 Q8): the conformance suite,
//! and replicas migrating one database together.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each harness works in a scratch schema of its own, and
//! every `open` is a new pool over it, as a restarted or second replica would create:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^ledger_store_postgres::/)'
//! ```

#![cfg(all(feature = "proc", feature = "postgres"))]

use std::str::FromStr;
use std::sync::Arc;

use adk_kernel::KernelError;
use adk_kernel::conformance::LedgerStoreHarness;
use adk_kernel::proc::{LedgerQuery, LedgerStore, PostgresLedgerStore};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// A scratch schema, created on first use.
struct Scratch {
    schema: String,
}

impl Scratch {
    fn new() -> Self {
        Self { schema: format!("adk_kernel_ledger_{}", uuid::Uuid::new_v4().simple()) }
    }

    /// Connection options whose search path is the scratch schema.
    async fn options(&self) -> PgConnectOptions {
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema)))
            .execute(&mut admin)
            .await
            .expect("create schema");
        PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", self.schema.as_str())])
    }

    /// A pool of its own, as a replica holds.
    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(4)
            .connect_with(self.options().await)
            .await
            .expect("connect")
    }
}

#[async_trait::async_trait]
impl LedgerStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn LedgerStore> {
        let store = PostgresLedgerStore::new(self.pool().await);
        store.migrate().await.expect("ledger schema migrates");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
}

/// Replicas starting together migrate one at a time: every migration succeeds and the
/// registry records the step once.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn replicas_starting_together_migrate_once() {
    let scratch = Arc::new(Scratch::new());
    scratch.options().await;
    let mut starts = Vec::new();
    for _ in 0..4 {
        let scratch = scratch.clone();
        starts.push(tokio::spawn(async move {
            PostgresLedgerStore::new(scratch.pool().await).migrate().await
        }));
    }
    for start in starts {
        start.await.expect("start completes").expect("migrate");
    }
    let pool = scratch.pool().await;
    let (applied,): (i64,) = sqlx::query_as("SELECT count(*) FROM _adk_kernel_ledger_migrations")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(applied, 1);
}

/// `connect` opens a pool of its own; an unmigrated schema answers with a store error.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn connect_opens_a_pool() {
    let store = PostgresLedgerStore::connect(&database_url()).await.expect("connect");
    let (one,): (i32,) = sqlx::query_as("SELECT 1").fetch_one(store.pool()).await.expect("query");
    assert_eq!(one, 1);
    let unmigrated = PostgresLedgerStore::new(Scratch::new().pool().await);
    let error = unmigrated.list(&LedgerQuery::default()).await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "postgres", .. }), "{error}");
}
