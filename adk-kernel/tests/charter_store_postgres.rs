//! The PostgreSQL charter, KPI, autonomy and signal stores (spec 3.5 §7.10, §7.11, §11):
//! the four conformance suites, and replicas migrating one database together.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each harness works in a scratch schema of its own, and
//! every `open` is a new pool over it, as a restarted or second replica would create:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels,charter \
//!   --run-ignored only --test integration -E 'test(/^charter_store_postgres::/)'
//! ```

#![cfg(all(feature = "charter", feature = "postgres"))]

use std::str::FromStr;
use std::sync::Arc;

use adk_kernel::charter::{
    AutonomyStore, CharterStore, KpiStore, PostgresAutonomyStore, PostgresCharterStore,
    PostgresKpiStore, PostgresSignalStore, SignalStore,
};
use adk_kernel::conformance::charter::{
    AutonomyStoreHarness, CharterStoreHarness, KpiStoreHarness, SignalStoreHarness,
};
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
        Self { schema: format!("adk_kernel_charter_{}", uuid::Uuid::new_v4().simple()) }
    }

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

    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(4)
            .connect_with(self.options().await)
            .await
            .expect("connect")
    }
}

#[async_trait::async_trait]
impl CharterStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn CharterStore> {
        let store = PostgresCharterStore::new(self.pool().await);
        store.migrate().await.expect("charter schema migrates");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl KpiStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn KpiStore> {
        let store = PostgresKpiStore::new(self.pool().await);
        store.migrate().await.expect("charter schema migrates");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl AutonomyStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn AutonomyStore> {
        let store = PostgresAutonomyStore::new(self.pool().await);
        store.migrate().await.expect("charter schema migrates");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl SignalStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn SignalStore> {
        let store = PostgresSignalStore::new(self.pool().await);
        store.migrate().await.expect("charter schema migrates");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::charter_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
    adk_kernel::kpi_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
    adk_kernel::autonomy_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
    adk_kernel::signal_store_conformance_tests! {
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
    for index in 0..4 {
        let scratch = scratch.clone();
        starts.push(tokio::spawn(async move {
            let pool = scratch.pool().await;
            if index % 2 == 0 {
                PostgresCharterStore::new(pool).migrate().await
            } else {
                PostgresSignalStore::new(pool).migrate().await
            }
        }));
    }
    for start in starts {
        start.await.expect("start completes").expect("migrate");
    }
    let pool = scratch.pool().await;
    let (applied,): (i64,) = sqlx::query_as("SELECT count(*) FROM _adk_kernel_charter_migrations")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(applied, 1);
}
