//! The Redis stores pass the shared conformance suites, and the run store drains one
//! queue under contention with every run claimed exactly once.
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
    ActionLedgerHarness, ApprovalStoreHarness, GovernanceStateStoreHarness, LeaseStoreHarness,
    RunStoreHarness, ScheduleStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{
    ActionLedger, ApprovalStore, Clock, DurableRunStore, GovernanceStateStore, LeaseStore,
    ScheduleStore, SpendLedger, async_trait,
};
use adk_session::{
    RedisActionLedger, RedisApprovalStore, RedisGovernanceStateStore, RedisLeaseStore,
    RedisRunStore, RedisScheduleStore, RedisSpendLedger,
};

#[path = "common/run_contention.rs"]
mod run_contention;

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

#[async_trait]
impl LeaseStoreHarness for Redis {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store =
            RedisLeaseStore::new(&redis_url()).await.expect("connect to redis").with_clock(clock);
        store.migrate().await.expect("load scripts");
        Arc::new(store)
    }
}

#[async_trait]
impl RunStoreHarness for Redis {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        let store =
            RedisRunStore::new(&redis_url()).await.expect("connect to redis").with_clock(clock);
        store.migrate().await.expect("load scripts");
        Arc::new(store)
    }
}

#[async_trait]
impl ScheduleStoreHarness for Redis {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = RedisScheduleStore::new(&redis_url()).await.expect("connect to redis");
        store.migrate().await.expect("load scripts");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for Redis {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = RedisGovernanceStateStore::new(&redis_url())
            .await
            .expect("connect to redis")
            .with_clock(clock);
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

mod approval_service {
    adk_core::approval_service_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod leases {
    adk_core::lease_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod runs {
    adk_core::run_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod schedules {
    adk_core::schedule_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

mod governance {
    adk_core::governance_state_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}

#[ignore = "requires a Redis server at REDIS_URL"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn run_claimers_drain_a_queue_exactly_once() {
    let first = RedisRunStore::new(&redis_url()).await.expect("connect to redis");
    let second = RedisRunStore::new(&redis_url()).await.expect("connect to redis");
    first.migrate().await.expect("load scripts");
    run_contention::claimers_drain_a_queue_exactly_once(Arc::new(first), Arc::new(second)).await;
}

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for Redis {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        let store =
            adk_session::RedisAuditLogStore::new(&redis_url()).await.expect("connect to redis");
        store.migrate().await.expect("set up the audit log store");
        Arc::new(store)
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests! {
        #[ignore = "requires a Redis server at REDIS_URL"]
        harness = super::Redis,
    }
}
