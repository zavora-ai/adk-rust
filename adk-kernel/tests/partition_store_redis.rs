//! The Redis partition store (spec 3.2 §7.1, §7.8, §11, §13 Q8): the conformance suite,
//! namespaces, and the layout version.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^partition_store_redis::/)'
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_kernel::conformance::PartitionStoreHarness;
use adk_kernel::{
    KernelError, Partition, PartitionId, PartitionStore, ProcessKind, ProcessMeta, ProcessStore,
    QueueClass, RedisPartitionStore, RedisProcessStore,
};
use fred::prelude::KeysInterface as _;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a client of its own, as a replica holds.
async fn store(namespace: &str) -> RedisPartitionStore {
    let store = RedisPartitionStore::connect(&redis_url())
        .await
        .expect("connect")
        .with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl PartitionStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn PartitionStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::partition_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two deployments under different namespaces never see each other's partitions, and the
/// process store under the same namespace keeps its own keys.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_deployments_apart() {
    let ns = namespace();
    let (first, second) = (store(&ns).await, store(&namespace()).await);
    let work = Partition::new("work", "work");
    first.put(work.clone()).await.expect("put");
    assert_eq!(first.get(&PartitionId::new("work")).await.expect("get"), Some(work));
    assert_eq!(second.get(&PartitionId::new("work")).await.expect("get"), None);
    assert!(second.list().await.expect("list").is_empty());
    let processes =
        RedisProcessStore::connect(&redis_url()).await.expect("connect").with_namespace(&ns);
    processes.migrate().await.expect("process layout");
    processes
        .put(ProcessMeta::new("work", QueueClass::Background, ProcessKind::Goal))
        .await
        .expect("put a process with the partition's id");
    assert_eq!(first.list().await.expect("list").len(), 1);
}

/// Keys a newer release laid out are refused rather than written in an old layout.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_newer_layout_is_refused() {
    let namespace = namespace();
    let store = store(&namespace).await;
    let _: () = store
        .client()
        .set(format!("{{{namespace}}}:partitions:layout"), "99", None, None, false)
        .await
        .expect("set");
    let error = store.migrate().await.expect_err("a newer layout");
    assert!(
        matches!(&error, KernelError::Store { backend: "redis", reason } if reason.contains("v99")),
        "{error}"
    );
}
