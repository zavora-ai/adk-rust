//! The Neo4j spend ledger, action ledger, and approval store pass the shared conformance
//! suites.
//!
//! Run with a server at `NEO4J_URI` (default `bolt://localhost:7687`), authenticating as
//! `NEO4J_USER` (default `neo4j`) with `NEO4J_PASSWORD`. Every case writes under fresh
//! identifiers, so the cases share one database:
//!
//! ```bash
//! NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-session --features neo4j --run-ignored only store_conformance_neo4j
//! ```

#![cfg(feature = "neo4j")]

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{ActionLedger, ApprovalStore, SpendLedger, async_trait};
use adk_session::{Neo4jActionLedger, Neo4jApprovalStore, Neo4jSpendLedger};
use neo4rs::Graph;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// A new connection per call, as a restarted or second process would open.
async fn graph() -> Graph {
    Graph::new(
        &env_or("NEO4J_URI", "bolt://localhost:7687"),
        &env_or("NEO4J_USER", "neo4j"),
        &env_or("NEO4J_PASSWORD", ""),
    )
    .await
    .expect("connect to neo4j")
}

struct Neo4j;

#[async_trait]
impl SpendLedgerHarness for Neo4j {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        let ledger = Neo4jSpendLedger::from_graph(graph().await)
            .with_limits(setup.limits)
            .with_reservation_ttl(setup.reservation_ttl)
            .with_clock(setup.clock);
        ledger.migrate().await.expect("migrate spend ledger schema");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ActionLedgerHarness for Neo4j {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        let ledger = Neo4jActionLedger::from_graph(graph().await);
        ledger.migrate().await.expect("migrate action ledger schema");
        Arc::new(ledger)
    }
}

#[async_trait]
impl ApprovalStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        let store = Neo4jApprovalStore::from_graph(graph().await);
        store.migrate().await.expect("migrate approval store schema");
        Arc::new(store)
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod actions {
    adk_core::action_ledger_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod approvals {
    adk_core::approval_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}
