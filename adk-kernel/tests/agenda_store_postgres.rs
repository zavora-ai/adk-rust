//! The PostgreSQL agenda store (spec 3.2 §7.5, §11, §13 Q8): the conformance suite.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each harness works in a scratch schema of its own, and
//! every `open` is a new pool over it, as a restarted or second replica would create:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test agenda_store_postgres
//! ```

#![cfg(feature = "postgres")]

use std::str::FromStr;
use std::sync::Arc;

use adk_kernel::conformance::AgendaStoreHarness;
use adk_kernel::{AgendaStore, PostgresAgendaStore};
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
        Self { schema: format!("adk_kernel_agenda_{}", uuid::Uuid::new_v4().simple()) }
    }

    async fn pool(&self) -> PgPool {
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema)))
            .execute(&mut admin)
            .await
            .expect("create schema");
        let options = PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("connect")
    }
}

#[async_trait::async_trait]
impl AgendaStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        let store = PostgresAgendaStore::new(self.pool().await);
        store.migrate().await.expect("agenda schema migrates");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
}
