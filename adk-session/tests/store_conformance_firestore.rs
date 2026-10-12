//! The Firestore spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, and governance state store pass the shared conformance suites, and
//! the run store passes a claim contention check across two clients.
//!
//! Run against the Firestore emulator at `FIRESTORE_EMULATOR_HOST`. Each case writes
//! under fresh identifiers, so the suites share one emulator database:
//!
//! ```bash
//! gcloud emulators firestore start --host-port=127.0.0.1:8080 &
//! FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
//!   cargo nextest run -p adk-session --features firestore --run-ignored only \
//!   --test store_conformance_firestore
//! ```
//!
//! `FIRESTORE_PROJECT_ID` names the emulator project (default `adk-conformance`).

#![cfg(feature = "firestore")]

#[path = "common/run_contention.rs"]
mod run_contention;

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, GovernanceStateStoreHarness, LeaseStoreHarness,
    RunStoreHarness, ScheduleStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{
    ActionLedger, ApprovalStore, Clock, DurableRunStore, GovernanceStateStore, LeaseStore,
    ScheduleStore, SpendLedger, async_trait,
};
use adk_session::firestore_stores::connect_firestore_emulator;
use adk_session::{
    FirestoreActionLedger, FirestoreApprovalStore, FirestoreGovernanceStateStore,
    FirestoreLeaseStore, FirestoreRunStore, FirestoreScheduleStore, FirestoreSpendLedger,
};
use firestore::FirestoreDb;

/// A new client per call, as a restarted process would create.
async fn emulator_db() -> FirestoreDb {
    let project =
        std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "adk-conformance".to_string());
    connect_firestore_emulator(&project).await.expect("connect to the Firestore emulator")
}

struct Spend;

#[async_trait]
impl SpendLedgerHarness for Spend {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = FirestoreSpendLedger::from_db(emulator_db().await)
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("migrate");
        Arc::new(ledger)
    }
}

struct Actions;

#[async_trait]
impl ActionLedgerHarness for Actions {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = FirestoreActionLedger::from_db(emulator_db().await);
        ledger.migrate().await.expect("migrate");
        Arc::new(ledger)
    }
}

struct Approvals;

#[async_trait]
impl ApprovalStoreHarness for Approvals {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = FirestoreApprovalStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Spend,
    );
}

mod actions {
    adk_core::action_ledger_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Actions,
    );
}

mod approvals {
    adk_core::approval_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Approvals,
    );
}

/// Opens the durable runtime stores, each on a new client.
struct Runtime;

#[async_trait]
impl LeaseStoreHarness for Runtime {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store = FirestoreLeaseStore::from_db(emulator_db().await).with_clock(clock);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait]
impl RunStoreHarness for Runtime {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        let store = FirestoreRunStore::from_db(emulator_db().await).with_clock(clock);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait]
impl ScheduleStoreHarness for Runtime {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = FirestoreScheduleStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for Runtime {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = FirestoreGovernanceStateStore::from_db(emulator_db().await).with_clock(clock);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod approval_service {
    adk_core::approval_service_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Approvals,
    );
}

mod leases {
    adk_core::lease_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Runtime,
    );
}

mod runs {
    adk_core::run_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Runtime,
    );
}

mod schedules {
    adk_core::schedule_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Runtime,
    );
}

mod governance {
    adk_core::governance_state_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Runtime,
    );
}

/// Claimers on two clients drain one queue, each run claimed exactly once.
#[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn run_claims_under_contention_are_exclusive() {
    let first: Arc<dyn DurableRunStore> = Arc::new(FirestoreRunStore::from_db(emulator_db().await));
    let second: Arc<dyn DurableRunStore> =
        Arc::new(FirestoreRunStore::from_db(emulator_db().await));
    // The emulator serializes transactions slowly, so the queue is smaller than on the
    // other backends; 16 claimers still contend for every run.
    run_contention::claimers_drain_runs_exactly_once(first, second, 16, 16).await;
}

struct Audit;

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for Audit {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        let store = adk_session::FirestoreAuditLogStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Audit,
    );
}
