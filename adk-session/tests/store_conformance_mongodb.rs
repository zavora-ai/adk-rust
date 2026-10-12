//! The MongoDB spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, governance state store, and audit log store pass the shared conformance suites against
//! a live server, and the run store drains a contended queue claiming each run once.
//!
//! Run with a server at `MONGODB_URL` (default `mongodb://localhost:27017`). A replica set
//! (one node is enough) exercises the run store's transactional enqueue; a standalone
//! server exercises its fallback. `MONGODB_REQUIRE_TRANSACTIONS=1` fails the
//! transactional case on a standalone server instead of skipping it. The suites use the
//! `adk_store_conformance` database, and every case writes under fresh identifiers:
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

/// Whether the transactional enqueue path is under test: skips on a standalone server
/// unless `MONGODB_REQUIRE_TRANSACTIONS=1`, which turns a standalone server into a failure.
async fn transactional(store: &MongoRunStore) -> bool {
    let supported = store.supports_transactions().await.expect("detect transactions");
    if !supported && std::env::var("MONGODB_REQUIRE_TRANSACTIONS").as_deref() == Ok("1") {
        panic!("MONGODB_REQUIRE_TRANSACTIONS=1 but the server at MONGODB_URL is standalone");
    }
    supported
}

/// On the transactional path, concurrent `Reject` enqueues of one concurrency key admit
/// exactly one run, and every `Existing` answer names a run that still exists — a losing
/// `Reject` never surfaces a run it then withdraws.
#[ignore = "requires a MongoDB replica set at MONGODB_URL"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn run_losing_reject_never_answers_existing_for_a_withdrawn_run() {
    use adk_core::run_store::{Enqueued, NewRun, OnConflict, RunFilter};

    let clock: Arc<dyn Clock> = Arc::new(adk_core::SystemClock);
    let first = Arc::new(run_store(clock.clone()).await);
    let second = Arc::new(run_store(clock).await);
    if !transactional(&first).await {
        return;
    }
    for round in 0..4 {
        let queue = format!("conformance:reject/ü/{}", uuid::Uuid::new_v4());
        let shared_key = format!("idem-{round}");
        let tasks: Vec<_> = (0..24)
            .map(|i| {
                let store = if i % 2 == 0 { first.clone() } else { second.clone() };
                // Half share one idempotency key, so a loser's run could be answered as
                // `Existing` to them; the rest each bring their own key.
                let key = if i % 2 == 0 { shared_key.clone() } else { format!("own-{i}") };
                let run =
                    NewRun::new(&queue, "app", "user", "agent", serde_json::json!({ "i": i }))
                        .with_idempotency_key(key)
                        .with_concurrency_key("thread", OnConflict::Reject);
                tokio::spawn(async move { store.enqueue(run).await.expect("enqueue") })
            })
            .collect();
        let mut created = Vec::new();
        let mut answered = Vec::new();
        for task in tasks {
            match task.await.expect("enqueue completes") {
                Enqueued::Created(record) => created.push(record.run_id),
                Enqueued::Existing(record) => answered.push(record.run_id),
                Enqueued::Rejected { blocking_run_id } => answered.push(blocking_run_id),
            }
        }
        assert_eq!(created.len(), 1, "one run is admitted to the key: {created:?}");
        for run_id in &answered {
            assert_eq!(run_id, &created[0], "every other caller is answered with the admitted run");
            assert!(first.get(run_id).await.expect("get").is_some(), "{run_id} still exists");
        }
        let stored = first.list(RunFilter::default().queue(&queue)).await.expect("list");
        assert_eq!(stored.len(), 1, "no withdrawn or second run remains");
    }
}

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for Mongo {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        let store = adk_session::MongoAuditLogStore::new(&url(), DATABASE)
            .await
            .expect("connect to mongodb");
        store.migrate().await.expect("set up the audit log store");
        Arc::new(store)
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests! {
        #[ignore = "requires a MongoDB server at MONGODB_URL"]
        harness = super::Mongo,
    }
}
