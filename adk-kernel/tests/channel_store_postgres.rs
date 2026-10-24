//! The PostgreSQL channel binding store (spec 3.3 §10 K1): the conformance suite and replicas
//! migrating one database together.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each harness works in a scratch schema of its own:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels --run-ignored only \
//!   --test channel_store_postgres
//! ```

#![cfg(all(feature = "channels", feature = "postgres"))]

use std::str::FromStr;
use std::sync::Arc;

use adk_kernel::channels::{ChannelBindingStore, PostgresChannelBindingStore};
use adk_kernel::conformance::ChannelBindingStoreHarness;
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
        Self { schema: format!("adk_kernel_channel_{}", uuid::Uuid::new_v4().simple()) }
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
impl ChannelBindingStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn ChannelBindingStore> {
        let store = PostgresChannelBindingStore::new(self.pool().await);
        store.migrate().await.expect("channel schema migrates");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::channel_binding_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
}

/// Replicas starting together migrate one at a time: the registry records the step once.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn replicas_starting_together_migrate_once() {
    let scratch = Arc::new(Scratch::new());
    scratch.options().await;
    let mut starts = Vec::new();
    for _ in 0..4 {
        let scratch = scratch.clone();
        starts.push(tokio::spawn(async move {
            PostgresChannelBindingStore::new(scratch.pool().await).migrate().await
        }));
    }
    for start in starts {
        start.await.expect("start completes").expect("migrate");
    }
    let pool = scratch.pool().await;
    let (applied,): (i64,) = sqlx::query_as("SELECT count(*) FROM _adk_kernel_channel_migrations")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(applied, 1);
}
