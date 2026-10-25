//! The Redis agent store (spec 3.3 §5.7, §10 K7): the conformance suite, namespaces, and the
//! layout version.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each harness keeps its keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second replica would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels --run-ignored only \
//!   --test agents_store_redis
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_kernel::KernelError;
use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentStore};
use adk_kernel::conformance::AgentStoreHarness;
use adk_kernel::stores::RedisAgentStore;
use fred::prelude::KeysInterface as _;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a client of its own, as a replica holds.
async fn store(namespace: &str) -> RedisAgentStore {
    let store =
        RedisAgentStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

/// A namespace of its own.
struct Namespace(String);

#[async_trait::async_trait]
impl AgentStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn AgentStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::agent_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Two deployments under different namespaces never see each other's agents.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_deployments_apart() {
    let (first, second) = (store(&namespace()).await, store(&namespace()).await);
    let record = AgentRecord::new(
        "finance",
        AgentDefinition::new("Finance", "default"),
        AgentRecordState::Active,
        AgentOrigin::ByHand,
        SubjectRef::human("alice"),
        chrono::Utc::now(),
    );
    assert!(first.put_if_version(record.clone(), 0).await.expect("put"));
    assert_eq!(first.get("finance").await.expect("get"), Some(record.clone()));
    assert_eq!(second.get("finance").await.expect("get"), None);
    assert!(second.list().await.expect("list").is_empty());
    assert!(second.put_if_version(record, 0).await.expect("the other namespace is free"));
}

/// Keys a newer release laid out are refused rather than written in an old layout.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_newer_layout_is_refused() {
    let namespace = namespace();
    let store = store(&namespace).await;
    let _: () = store
        .client()
        .set(format!("{{{namespace}}}:agents:layout"), "99", None, None, false)
        .await
        .expect("set");
    let error = store.migrate().await.expect_err("a newer layout");
    assert!(
        matches!(&error, KernelError::Store { backend: "redis", reason } if reason.contains("v99")),
        "{error}"
    );
}
