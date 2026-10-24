//! The Redis control store (spec 3.3 §10 K3, K4): the conformance suite and the layout
//! version.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels --run-ignored only \
//!   --test control_store_redis
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_kernel::KernelError;
use adk_kernel::conformance::ControlStoreHarness;
use adk_kernel::control::{ControlStore, RedisControlStore};
use fred::prelude::KeysInterface as _;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-kernel-control-test:{}", uuid::Uuid::new_v4().simple())
}

async fn store(namespace: &str) -> RedisControlStore {
    let store =
        RedisControlStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace);
    store.migrate().await.expect("migrate");
    store
}

struct Namespace(String);

#[async_trait::async_trait]
impl ControlStoreHarness for Namespace {
    async fn open(&self) -> Arc<dyn ControlStore> {
        Arc::new(store(&self.0).await)
    }
}

mod conformance {
    use super::*;

    adk_kernel::control_store_conformance_tests! {
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        harness = Namespace(namespace())
    }
}

/// Keys a newer release laid out are refused rather than written in an old layout.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_newer_layout_is_refused() {
    let namespace = namespace();
    let store = store(&namespace).await;
    let _: () = store
        .client()
        .set(format!("{{{namespace}}}:control-layout"), "99", None, None, false)
        .await
        .expect("set");
    let error = store.migrate().await.expect_err("a newer layout");
    assert!(
        matches!(&error, KernelError::Store { backend: "redis", reason } if reason.contains("v99")),
        "{error}"
    );
}
