//! The PostgreSQL edge stores (spec 3.5 §7.9): the edge store conformance suite.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them through
//! `scripts/test-postgres.sh`. Each test keeps its records under a site of its own, and every
//! handle is a new pool:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-server --features awp-sqlite,awp-postgres,awp-redis \
//!   --run-ignored only --test awp_store_postgres
//! ```

#![cfg(feature = "awp-postgres")]

use std::sync::Arc;

use adk_awp::v2::store::conformance::EdgeStoreHarness;
use adk_awp::v2::store::{
    ConsentReceiptStore, HeldCallStore, IdempotencyStore, LeadStore, StatusStore, SubscriptionStore,
};
use adk_server::awp::stores::PostgresEdgeStores;
use async_trait::async_trait;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_owned())
}

/// A site namespace of its own.
struct Site(String);

impl Site {
    fn new() -> Self {
        Self(format!("edge-test-{}", uuid::Uuid::new_v4().simple()))
    }

    async fn stores(&self) -> PostgresEdgeStores {
        let stores =
            PostgresEdgeStores::connect(&database_url()).await.expect("connect").with_site(&self.0);
        stores.migrate().await.expect("migrate");
        stores
    }
}

#[async_trait]
impl EdgeStoreHarness for Site {
    async fn subscriptions(&self) -> Arc<dyn SubscriptionStore> {
        Arc::new(self.stores().await)
    }
    async fn consent(&self) -> Arc<dyn ConsentReceiptStore> {
        Arc::new(self.stores().await)
    }
    async fn status(&self) -> Arc<dyn StatusStore> {
        Arc::new(self.stores().await)
    }
    async fn held_calls(&self) -> Arc<dyn HeldCallStore> {
        Arc::new(self.stores().await)
    }
    async fn leads(&self) -> Arc<dyn LeadStore> {
        Arc::new(self.stores().await)
    }
    async fn idempotency(&self) -> Arc<dyn IdempotencyStore> {
        Arc::new(self.stores().await)
    }
}

adk_awp::edge_store_conformance_tests! {
    #[ignore = "needs PostgreSQL: DATABASE_URL"]
    harness = Site::new(),
}
