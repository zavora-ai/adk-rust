//! The Redis ledger store (spec 3.2 §7.1, §7.10, §11, §13 Q8): the conformance suite,
//! namespaces, and the layout version.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test ledger_store_redis
//! ```

#![cfg(all(feature = "proc", feature = "redis"))]

use std::sync::Arc;

use adk_kernel::KernelError;
use adk_kernel::conformance::LedgerStoreHarness;
use adk_kernel::proc::{
    Activity, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink, LedgerStore, RedisLedgerStore,
};
use fred::prelude::KeysInterface as _;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-ledger-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a client of its own, as a replica holds.
async fn store(namespace: &str) -> RedisLedgerStore {
    let store =
        RedisLedgerStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl LedgerStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn LedgerStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two deployments on one server, under different namespaces, never see each other's rows.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_deployments_apart() {
    let (first, second) = (Arc::new(store(&namespace()).await), store(&namespace()).await);
    let ledger = Ledger::new(first.clone(), LedgerKey::from_bytes([1; 32]));
    ledger.record(Activity::new(LedgerKind::Intent, "harness")).await.expect("record");
    assert_eq!(first.list(&LedgerQuery::default()).await.expect("list").len(), 1);
    assert!(second.list(&LedgerQuery::default()).await.expect("list").is_empty());
}

/// Keys a newer release laid out are refused rather than written in an old layout.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_newer_layout_is_refused() {
    let namespace = namespace();
    let store = store(&namespace).await;
    let _: () = store
        .client()
        .set(format!("{{{namespace}}}:ledger:layout"), "99", None, None, false)
        .await
        .expect("set");
    let error = store.migrate().await.expect_err("a newer layout");
    assert!(
        matches!(&error, KernelError::Store { backend: "redis", reason } if reason.contains("v99")),
        "{error}"
    );
}
