//! The SQLite spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, and governance state store pass the shared conformance suites. Each test uses its own database file, and every `open` is a new
//! connection pool, as a restarted process would create.

#![cfg(feature = "sqlite")]

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
    SqliteActionLedger, SqliteApprovalStore, SqliteGovernanceStateStore, SqliteLeaseStore,
    SqliteRunStore, SqliteScheduleStore, SqliteSpendLedger,
};

/// A database file, so pooled connections contend for the write lock as separate
/// processes would. Removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-conformance-{}.db", uuid::Uuid::new_v4()));
        Self { path }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for FileDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

#[async_trait]
impl SpendLedgerHarness for FileDatabase {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = SqliteSpendLedger::new(&self.url())
            .await
            .expect("SQLite opens")
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("spend schema migrates");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ActionLedgerHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = SqliteActionLedger::new(&self.url()).await.expect("SQLite opens");
        ledger.migrate().await.expect("action ledger schema migrates");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ApprovalStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = SqliteApprovalStore::new(&self.url()).await.expect("SQLite opens");
        store.migrate().await.expect("approval schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl LeaseStoreHarness for FileDatabase {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store =
            SqliteLeaseStore::new(&self.url()).await.expect("SQLite opens").with_clock(clock);
        store.migrate().await.expect("lease schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl RunStoreHarness for FileDatabase {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        let store = SqliteRunStore::new(&self.url()).await.expect("SQLite opens").with_clock(clock);
        store.migrate().await.expect("run schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl ScheduleStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = SqliteScheduleStore::new(&self.url()).await.expect("SQLite opens");
        store.migrate().await.expect("schedule schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for FileDatabase {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = SqliteGovernanceStateStore::new(&self.url())
            .await
            .expect("SQLite opens")
            .with_clock(clock);
        store.migrate().await.expect("governance schema migrates");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests!(harness = super::FileDatabase::new());
}

mod actions {
    adk_core::action_ledger_conformance_tests!(harness = super::FileDatabase::new());
}

mod approvals {
    adk_core::approval_store_conformance_tests!(harness = super::FileDatabase::new());
}

mod leases {
    adk_core::lease_store_conformance_tests!(harness = super::FileDatabase::new());
}

mod runs {
    adk_core::run_store_conformance_tests!(harness = super::FileDatabase::new());
}

mod schedules {
    adk_core::schedule_store_conformance_tests!(harness = super::FileDatabase::new());
}

mod governance {
    adk_core::governance_state_store_conformance_tests!(harness = super::FileDatabase::new());
}
