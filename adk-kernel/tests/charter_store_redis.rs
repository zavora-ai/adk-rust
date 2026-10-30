//! The Redis charter, KPI, autonomy and signal stores (spec 3.5 §7.10, §7.11, §11): the
//! four conformance suites and namespaces.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels,charter \
//!   --run-ignored only --test integration -E 'test(/^charter_store_redis::/)'
//! ```

#![cfg(all(feature = "charter", feature = "redis"))]

use std::sync::Arc;

use adk_kernel::charter::{
    AutonomyStore, CharterStore, KpiSample, KpiStore, RedisAutonomyStore, RedisCharterStore,
    RedisKpiStore, RedisSignalStore, SignalStore,
};
use adk_kernel::conformance::charter::{
    AutonomyStoreHarness, CharterStoreHarness, KpiStoreHarness, SignalStoreHarness,
};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-test:{}", uuid::Uuid::new_v4().simple())
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl CharterStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn CharterStore> {
        let store = RedisCharterStore::connect(&redis_url())
            .await
            .expect("connect")
            .with_namespace(&self.0);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl KpiStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn KpiStore> {
        let store =
            RedisKpiStore::connect(&redis_url()).await.expect("connect").with_namespace(&self.0);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl AutonomyStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn AutonomyStore> {
        let store = RedisAutonomyStore::connect(&redis_url())
            .await
            .expect("connect")
            .with_namespace(&self.0);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl SignalStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn SignalStore> {
        let store =
            RedisSignalStore::connect(&redis_url()).await.expect("connect").with_namespace(&self.0);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::charter_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
    adk_kernel::kpi_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
    adk_kernel::autonomy_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
    adk_kernel::signal_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two tenants under different namespaces never see each other's samples.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_tenants_apart() {
    let open = |namespace: String| async move {
        let store =
            RedisKpiStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace);
        store.migrate().await.expect("migrate");
        store
    };
    let (first, second) = (open(namespace()).await, open(namespace()).await);
    let site = uuid::Uuid::nil();
    let now = chrono::Utc::now();
    first.record(KpiSample::new(site, "leads", now, 1.0, "commerce")).await.expect("record");
    let window = (now - chrono::Duration::hours(1), now + chrono::Duration::hours(1));
    assert_eq!(first.samples(site, "leads", window.0, window.1).await.expect("samples").len(), 1);
    assert!(second.samples(site, "leads", window.0, window.1).await.expect("samples").is_empty());
}
