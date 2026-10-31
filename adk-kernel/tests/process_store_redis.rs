//! The Redis process store (spec 3.2 §7.1, §11, §13 Q8): the conformance suite, namespaces,
//! and the layout version.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^process_store_redis::/)'
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_kernel::conformance::ProcessStoreHarness;
use adk_kernel::{
    KernelError, ProcessKind, ProcessMeta, ProcessStore, QueueClass, RedisProcessStore,
};
use fred::prelude::KeysInterface as _;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a client of its own, as a replica holds.
async fn store(namespace: &str) -> RedisProcessStore {
    let store =
        RedisProcessStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl ProcessStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::process_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two deployments on one server, under different namespaces, never see each other's rows.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_deployments_apart() {
    let (first, second) = (store(&namespace()).await, store(&namespace()).await);
    let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal);
    first.put(meta.clone()).await.expect("put");
    assert_eq!(first.get("run-1").await.expect("get"), Some(meta));
    assert_eq!(second.get("run-1").await.expect("get"), None);
    assert!(second.list(&Default::default()).await.expect("list").is_empty());
    assert!(!second.delete("run-1").await.expect("delete"));
}

/// The default namespace is `adk:kernel`, and braces in a namespace cannot move the hash
/// tag: the row sits under `{<namespace>}` with the braces encoded.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn keys_sit_under_the_namespace_hash_tag() {
    let base = RedisProcessStore::connect(&redis_url()).await.expect("connect");
    assert_eq!(base.namespace(), RedisProcessStore::DEFAULT_NAMESPACE);
    let namespace = format!("{}{{tag}}", namespace());
    let store = base.with_namespace(namespace.clone());
    store.migrate().await.expect("migrate");
    store
        .put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal))
        .await
        .expect("put");
    let encoded = namespace.replace('{', "%7B").replace('}', "%7D");
    let exists: i64 =
        store.client().exists(format!("{{{encoded}}}:process:run-1")).await.expect("exists");
    assert_eq!(exists, 1);
}

/// Keys a newer release laid out are refused rather than written in an old layout.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_newer_layout_is_refused() {
    let namespace = namespace();
    let store = store(&namespace).await;
    let _: () = store
        .client()
        .set(format!("{{{namespace}}}:layout"), "99", None, None, false)
        .await
        .expect("set");
    let error = store.migrate().await.expect_err("a newer layout");
    assert!(
        matches!(&error, KernelError::Store { backend: "redis", reason } if reason.contains("v99")),
        "{error}"
    );
}
