//! The Redis cron watermark store (spec 3.2 §7.4, §11, §13 Q8): the conformance suite and
//! namespaces.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^cron_watermark_store_redis::/)'
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_kernel::conformance::CronWatermarkStoreHarness;
use adk_kernel::{CronWatermarkStore, RedisCronWatermarkStore};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a client of its own, as a replica holds.
async fn store(namespace: &str) -> RedisCronWatermarkStore {
    let store = RedisCronWatermarkStore::connect(&redis_url())
        .await
        .expect("connect")
        .with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl CronWatermarkStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn CronWatermarkStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::cron_watermark_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two tenants under different namespaces never see each other's cursors.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_tenants_apart() {
    let (first, second) = (store(&namespace()).await, store(&namespace()).await);
    let now = chrono::Utc::now();
    first.advance("0_0_9_*_*_*", now).await.expect("advance");
    assert!(first.read("0_0_9_*_*_*").await.expect("read").is_some());
    assert_eq!(second.read("0_0_9_*_*_*").await.expect("read"), None);
}
