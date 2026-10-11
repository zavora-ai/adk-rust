//! The MongoDB spend ledger, action ledger, and approval store pass the shared
//! conformance suites against a live server.
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
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{ActionLedger, ApprovalStore, SpendLedger, async_trait};
use adk_session::{MongoActionLedger, MongoApprovalStore, MongoSpendLedger};

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
