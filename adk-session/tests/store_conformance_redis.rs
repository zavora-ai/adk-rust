//! The Redis stores pass the shared conformance suites.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`). Every case
//! writes under fresh identifiers, so a shared server is safe:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-session --features redis --run-ignored only \
//!   --test store_conformance_redis
//! ```

#![cfg(feature = "redis")]

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{ActionLedger, ApprovalStore, SpendLedger, async_trait};
use adk_session::{RedisActionLedger, RedisApprovalStore, RedisSpendLedger};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

/// Each `open` connects a new client, as a restarted process would.
struct Redis;

#[async_trait]
impl SpendLedgerHarness for Redis {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = RedisSpendLedger::new(&redis_url())
            .await
            .expect("connect to redis")
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("load scripts");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ActionLedgerHarness for Redis {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = RedisActionLedger::new(&redis_url()).await.expect("connect to redis");
        ledger.migrate().await.expect("load scripts");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ApprovalStoreHarness for Redis {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = RedisApprovalStore::new(&redis_url()).await.expect("connect to redis");
        store.migrate().await.expect("load scripts");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod actions {
    adk_core::action_ledger_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod approvals {
    adk_core::approval_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}
