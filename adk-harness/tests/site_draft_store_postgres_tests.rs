//! The website draft store on PostgreSQL (spec 3.5 §7.8.2): the conformance suite, two
//! handles over one database and a restart included.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each test works in a scratch schema of its own:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --features website,postgres --run-ignored only \
//!   --test integration -E 'test(/^site_draft_store_postgres_tests::/)'
//! ```

#![cfg(all(feature = "website", feature = "postgres"))]

use std::str::FromStr;
use std::sync::Arc;

use adk_harness::website::draft_conformance::SiteDraftStoreHarness;
use adk_harness::website::{PostgresSiteDraftStore, SiteDraftStore};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// A scratch schema every `open` connects to with a pool of its own, as a replica does.
struct Postgres(PgConnectOptions);

impl Postgres {
    async fn scratch() -> Self {
        let schema = format!("adk_website_drafts_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        Self(
            PgConnectOptions::from_str(&database_url())
                .expect("parse DATABASE_URL")
                .options([("search_path", schema.as_str())]),
        )
    }
}

#[async_trait::async_trait]
impl SiteDraftStoreHarness for Postgres {
    async fn open(&self) -> Arc<dyn SiteDraftStore> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(self.0.clone())
            .await
            .expect("connect");
        let store = PostgresSiteDraftStore::new(pool);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

adk_harness::site_draft_store_conformance_tests!(
    #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
    harness = Postgres::scratch().await
);
