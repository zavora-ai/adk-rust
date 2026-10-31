//! The website draft store on Redis (spec 3.5 §7.8.2): the conformance suite, two handles
//! over one server and a restart included.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each test keeps its keys under a namespace of its own:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-harness --features website,redis --run-ignored only \
//!   --test integration -E 'test(/^site_draft_store_redis_tests::/)'
//! ```

#![cfg(all(feature = "website", feature = "redis"))]

use std::sync::Arc;

use adk_harness::website::draft_conformance::SiteDraftStoreHarness;
use adk_harness::website::{RedisSiteDraftStore, SiteDraftStore};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

/// A namespace every `open` connects to with a client of its own, as a replica does.
struct Redis(String);

impl Redis {
    fn scratch() -> Self {
        Self(format!("adk-website-drafts-test:{}", uuid::Uuid::new_v4().simple()))
    }
}

#[async_trait::async_trait]
impl SiteDraftStoreHarness for Redis {
    async fn open(&self) -> Arc<dyn SiteDraftStore> {
        let store = RedisSiteDraftStore::connect(&redis_url())
            .await
            .expect("connect")
            .with_namespace(self.0.clone());
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

adk_harness::site_draft_store_conformance_tests!(
    #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
    harness = Redis::scratch()
);
