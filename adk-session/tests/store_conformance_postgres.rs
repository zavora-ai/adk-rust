//! The PostgreSQL spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, and governance state store pass the shared conformance suites against
//! a live server.
//!
//! Every `open` is a new connection pool, as a restarted process would create. Cases
//! write under fresh identifiers, so they share one scratch database at `DATABASE_URL`
//! (default `postgres://postgres@localhost:5432/postgres`):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only \
//!   --test store_conformance_postgres
//! ```

#![cfg(feature = "postgres")]

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
    PostgresActionLedger, PostgresApprovalStore, PostgresGovernanceStateStore, PostgresLeaseStore,
    PostgresRunStore, PostgresScheduleStore, PostgresSpendLedger,
};
use sqlx::postgres::{PgPool, PgPoolOptions};

struct Postgres;

impl Postgres {
    async fn pool(&self) -> PgPool {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
        PgPoolOptions::new().max_connections(4).connect(&url).await.expect("connect to postgres")
    }
}

#[async_trait]
impl SpendLedgerHarness for Postgres {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = PostgresSpendLedger::from_pool(self.pool().await)
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("spend schema migrates");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ActionLedgerHarness for Postgres {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = PostgresActionLedger::from_pool(self.pool().await);
        ledger.migrate().await.expect("action ledger schema migrates");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ApprovalStoreHarness for Postgres {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = PostgresApprovalStore::from_pool(self.pool().await);
        store.migrate().await.expect("approval schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl LeaseStoreHarness for Postgres {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store = PostgresLeaseStore::from_pool(self.pool().await).with_clock(clock);
        store.migrate().await.expect("lease schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl RunStoreHarness for Postgres {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        let store = PostgresRunStore::from_pool(self.pool().await).with_clock(clock);
        store.migrate().await.expect("run schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl ScheduleStoreHarness for Postgres {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = PostgresScheduleStore::from_pool(self.pool().await);
        store.migrate().await.expect("schedule schema migrates");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for Postgres {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = PostgresGovernanceStateStore::from_pool(self.pool().await).with_clock(clock);
        store.migrate().await.expect("governance schema migrates");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod actions {
    adk_core::action_ledger_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod approvals {
    adk_core::approval_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod leases {
    adk_core::lease_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod runs {
    adk_core::run_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod schedules {
    adk_core::schedule_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod governance {
    adk_core::governance_state_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for Postgres {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        let store = adk_session::PostgresAuditLogStore::from_pool(self.pool().await);
        store.migrate().await.expect("audit log schema migrates");
        Arc::new(store)
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}

mod approval_service {
    adk_core::approval_service_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = super::Postgres,
    );
}
