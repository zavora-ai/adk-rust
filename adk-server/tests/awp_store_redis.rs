//! The Redis edge stores (spec 3.5 §7.9): the edge store conformance suite.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with its
//! Redis service. Each test keeps its records under a site of its own, and every handle is a
//! new client:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-server --features awp-sqlite,awp-postgres,awp-redis \
//!   --run-ignored only --test awp_store_redis
//! ```

#![cfg(feature = "awp-redis")]

use std::sync::Arc;

use adk_awp::v2::store::conformance::EdgeStoreHarness;
use adk_awp::v2::store::{
    ConsentReceiptStore, HeldCallStore, IdempotencyStore, LeadStore, StatusStore, SubscriptionStore,
};
use adk_server::awp::stores::RedisEdgeStores;
use async_trait::async_trait;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_owned())
}

/// A site namespace of its own.
struct Site(String);

impl Site {
    fn new() -> Self {
        Self(format!("edge-test-{}", uuid::Uuid::new_v4().simple()))
    }

    async fn stores(&self) -> RedisEdgeStores {
        let stores =
            RedisEdgeStores::connect(&redis_url()).await.expect("connect").with_site(&self.0);
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
    #[ignore = "needs Redis: REDIS_URL"]
    harness = Site::new(),
}
