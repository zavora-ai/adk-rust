//! The Redis wallet store (spec 3.5 §7.14, A-I16): the conformance suite, a restart, a cap
//! and a key shared by two instances, and the layout guard.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each test keeps the store's keys under a namespace of its own, and every
//! `open` is a new client, as a restarted or second instance would create:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-payments --features sqlite,postgres,redis \
//!   --run-ignored only --test wallet_store_redis
//! ```

#![cfg(feature = "redis")]

#[path = "support/wallet_stores.rs"]
mod wallet_stores;

use std::sync::Arc;

use adk_core::{Clock, SpendLimits};
use adk_payments::wallet::{RedisWalletStore, WALLET_STORE_CORRUPT_CODE};
use adk_session::RedisSpendLedger;
use fred::prelude::KeysInterface as _;
use wallet_stores::{Backend, Instance};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

/// A namespace of its own; the spend ledger's keys are scoped by the wallet id, which every
/// scenario makes unique.
struct Namespace(String);

impl Namespace {
    fn new() -> Self {
        Self(format!("adk-wallet-test:{}", uuid::Uuid::new_v4().simple()))
    }
}

#[async_trait::async_trait]
impl Backend for Namespace {
    async fn open(&self, limits: SpendLimits, clock: Arc<dyn Clock>) -> Instance {
        let store =
            RedisWalletStore::connect(&redis_url()).await.expect("connect").with_namespace(&self.0);
        store.migrate().await.expect("the layout is recorded");
        let ledger = RedisSpendLedger::new(&redis_url())
            .await
            .expect("connect")
            .with_limits(limits)
            .with_clock(clock);
        ledger.migrate().await.expect("the ledger scripts load");
        Instance { store: Arc::new(store), ledger: Arc::new(ledger) }
    }
}

#[tokio::test]
#[ignore = "needs Redis: REDIS_URL"]
async fn the_redis_store_passes_the_conformance_suite() {
    wallet_stores::conformance(&Namespace::new()).await;
}

#[tokio::test]
#[ignore = "needs Redis: REDIS_URL"]
async fn a_held_payment_and_a_paused_category_survive_a_restart() {
    wallet_stores::held_payment_and_pause_survive_a_restart(&Namespace::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Redis: REDIS_URL"]
async fn two_instances_never_settle_past_a_cap() {
    wallet_stores::instances_never_settle_past_the_cap(&Namespace::new(), 4, 40).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Redis: REDIS_URL"]
async fn a_key_pays_once_across_two_instances() {
    wallet_stores::a_key_pays_once_across_instances(&Namespace::new(), 16).await;
}

#[tokio::test]
#[ignore = "needs Redis: REDIS_URL"]
async fn a_newer_layout_is_refused() {
    let namespace = Namespace::new();
    let store = RedisWalletStore::connect(&redis_url()).await.unwrap().with_namespace(&namespace.0);
    store.migrate().await.expect("the first migration records the layout");
    store.migrate().await.expect("migrating again is a no-op");
    store
        .client()
        .set::<(), _, _>(format!("{}:layout", namespace.0), 99, None, None, false)
        .await
        .unwrap();
    let error = store.migrate().await.expect_err("a newer layout is refused");
    assert_eq!(error.code, WALLET_STORE_CORRUPT_CODE);
}
