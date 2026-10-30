//! The Redis agenda store (spec 3.2 §7.5, §11, §13 Q8): the conformance suite.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^agenda_store_redis::/)'
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_kernel::conformance::AgendaStoreHarness;
use adk_kernel::{AgendaStore, RedisAgendaStore};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl AgendaStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        let store = RedisAgendaStore::connect(&redis_url())
            .await
            .expect("connect")
            .with_namespace(self.0.clone());
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(format!("adk-kernel-agenda-test:{}", uuid::Uuid::new_v4().simple()))
    }
}
