//! The Firestore stores pass the shared conformance suites.
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

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{ActionLedger, ApprovalStore, SpendLedger, async_trait};
use adk_session::firestore_stores::connect_firestore_emulator;
use adk_session::{FirestoreActionLedger, FirestoreApprovalStore, FirestoreSpendLedger};
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

mod approval_service {
    adk_core::approval_service_conformance_tests!(
        #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
        harness = super::Approvals,
    );
}
