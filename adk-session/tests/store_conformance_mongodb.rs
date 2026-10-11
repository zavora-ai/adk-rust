//! The MongoDB spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, and governance state store pass the shared conformance suites against
//! a live server, and the run store drains a contended queue claiming each run once.
//!
//! Run with a server at `MONGODB_URL` (default `mongodb://localhost:27017`); a standalone
//! server is enough. The suites use the `adk_store_conformance` database, and every case
//! writes under fresh identifiers:
//!
//! ```bash
//! MONGODB_URL=mongodb://localhost:27017 \
//!   cargo nextest run -p adk-session --features mongodb --run-ignored only \
//!   --test store_conformance_mongodb
//! ```

#![cfg(feature = "mongodb")]

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
    MongoActionLedger, MongoApprovalStore, MongoGovernanceStateStore, MongoLeaseStore,
    MongoRunStore, MongoScheduleStore, MongoSpendLedger,
};

#[path = "common/run_contention.rs"]
mod run_contention;

const DATABASE: &str = "adk_store_conformance";

fn url() -> String {
    std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://localhost:27017".to_string())
}

/// Each `open` connects a new client, as a restarted process would.
struct Mongo;

#[async_trait]
impl SpendLedgerHarness for Mongo {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = MongoSpendLedger::new(&url(), DATABASE)
            .await
            .expect("connect to mongodb")
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("create spend indexes");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ActionLedgerHarness for Mongo {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = MongoActionLedger::new(&url(), DATABASE).await.expect("connect to mongodb");
        ledger.migrate().await.expect("reach mongodb");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ApprovalStoreHarness for Mongo {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = MongoApprovalStore::new(&url(), DATABASE).await.expect("connect to mongodb");
        store.migrate().await.expect("create approval indexes");
        Arc::new(store)
    }
}

#[async_trait]
impl LeaseStoreHarness for Mongo {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store = MongoLeaseStore::new(&url(), DATABASE)
            .await
            .expect("connect to mongodb")
            .with_clock(clock);
        store.migrate().await.expect("reach mongodb");
        Arc::new(store)
    }
}

async fn run_store(clock: Arc<dyn Clock>) -> MongoRunStore {
    let store =
        MongoRunStore::new(&url(), DATABASE).await.expect("connect to mongodb").with_clock(clock);
    store.migrate().await.expect("create run indexes");
    store
}

#[async_trait]
impl RunStoreHarness for Mongo {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        Arc::new(run_store(clock).await)
    }
}

#[async_trait]
impl ScheduleStoreHarness for Mongo {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = MongoScheduleStore::new(&url(), DATABASE).await.expect("connect to mongodb");
        store.migrate().await.expect("create schedule index");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for Mongo {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = MongoGovernanceStateStore::new(&url(), DATABASE)
            .await
            .expect("connect to mongodb")
            .with_clock(clock);
        store.migrate().await.expect("reach mongodb");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod actions {
    adk_core::action_ledger_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod approvals {
    adk_core::approval_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod approval_service {
    adk_core::approval_service_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod leases {
    adk_core::lease_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod runs {
    adk_core::run_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod schedules {
    adk_core::schedule_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

mod governance {
    adk_core::governance_state_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}

#[ignore = "requires a MongoDB server at MONGODB_URL"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn run_claimers_drain_a_contended_queue_exactly_once() {
    let clock: Arc<dyn Clock> = Arc::new(adk_core::SystemClock);
    let first = Arc::new(run_store(clock.clone()).await);
    let second = Arc::new(run_store(clock).await);
    run_contention::claimers_drain_a_queue_exactly_once(first, second).await;
}
