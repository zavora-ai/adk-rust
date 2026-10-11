//! The PostgreSQL spend ledger, action ledger, and approval store pass the shared
//! conformance suites against a live server.
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
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{ActionLedger, ApprovalStore, SpendLedger, async_trait};
use adk_session::{PostgresActionLedger, PostgresApprovalStore, PostgresSpendLedger};
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
