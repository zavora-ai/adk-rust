//! The Neo4j spend ledger, action ledger, approval store, lease store, run store,
//! schedule store, governance state store, and audit log store pass the shared conformance suites, and the
//! run store drains one queue under contention with every run claimed exactly once.
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
    ActionLedgerHarness, ApprovalStoreHarness, GovernanceStateStoreHarness, LeaseStoreHarness,
    RunStoreHarness, ScheduleStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{
    ActionLedger, ApprovalStore, Clock, DurableRunStore, GovernanceStateStore, LeaseStore,
    ScheduleStore, SpendLedger, async_trait,
};
use adk_session::{
    Neo4jActionLedger, Neo4jApprovalStore, Neo4jGovernanceStateStore, Neo4jLeaseStore,
    Neo4jRunStore, Neo4jScheduleStore, Neo4jSpendLedger,
};
use neo4rs::Graph;

#[path = "common/run_contention.rs"]
mod run_contention;

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

#[async_trait]
impl LeaseStoreHarness for Neo4j {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        let store = Neo4jLeaseStore::from_graph(graph().await).with_clock(clock);
        store.migrate().await.expect("migrate lease store schema");
        Arc::new(store)
    }
}

async fn run_store(clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
    let store = Neo4jRunStore::from_graph(graph().await).with_clock(clock);
    store.migrate().await.expect("migrate run store schema");
    Arc::new(store)
}

#[async_trait]
impl RunStoreHarness for Neo4j {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        run_store(clock).await
    }
}

#[async_trait]
impl ScheduleStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        let store = Neo4jScheduleStore::from_graph(graph().await);
        store.migrate().await.expect("migrate schedule store schema");
        Arc::new(store)
    }
}

#[async_trait]
impl GovernanceStateStoreHarness for Neo4j {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        let store = Neo4jGovernanceStateStore::from_graph(graph().await).with_clock(clock);
        store.migrate().await.expect("migrate governance store schema");
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

mod approval_service {
    adk_core::approval_service_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod leases {
    adk_core::lease_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod runs {
    adk_core::run_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod schedules {
    adk_core::schedule_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

mod governance {
    adk_core::governance_state_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Neo4j,
    }
}

#[ignore = "requires a Neo4j server at NEO4J_URI"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn runs_contention_claims_each_run_once() {
    let clock: Arc<dyn Clock> = Arc::new(adk_core::SystemClock);
    let first = run_store(clock.clone()).await;
    let second = run_store(clock).await;
    run_contention::claimers_drain_a_queue_exactly_once(first, second).await;
}

struct Audit;

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for Audit {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        let store = adk_session::Neo4jAuditLogStore::from_graph(graph().await);
        store.migrate().await.expect("migrate audit log schema");
        Arc::new(store)
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests! {
        #[ignore = "requires a Neo4j server at NEO4J_URI"]
        harness = super::Audit,
    }
}

/// Callers drop store futures mid-query: the durable worker drops its in-flight heartbeat
/// when an attempt ends. A dropped call must not hand a pooled connection back halfway
/// through a bolt exchange, or the next call on it reads the stale response
/// (`unexpected response for RUN`) and a fenced write fails or is misread.
#[ignore = "requires a Neo4j server at NEO4J_URI"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_dropped_mid_query_leave_the_pool_usable() {
    use std::time::Duration;

    use adk_core::{NewRun, RunOutcome, RunState};

    let store = Neo4jRunStore::from_graph(graph().await);
    store.migrate().await.expect("migrate run store schema");
    let queue = format!("dropped-{}", uuid::Uuid::new_v4());
    let ttl = Duration::from_secs(30);
    store
        .enqueue(NewRun::new(&queue, "app", "user", "agent", serde_json::json!({})))
        .await
        .expect("enqueue");
    let claimed = store.claim(&queue, "worker", ttl).await.expect("claim").expect("claimable");
    let run_id = claimed.record.run_id.clone();

    for i in 0..300u64 {
        // Drops the heartbeat at a different point of its exchange each time.
        let cut = Duration::from_micros((i % 40) * 75);
        let _ = tokio::time::timeout(cut, store.heartbeat(&run_id, claimed.epoch, ttl)).await;
        let record = store
            .get(&run_id)
            .await
            .unwrap_or_else(|error| panic!("read after dropped call {i} failed: {error}"))
            .expect("run exists");
        assert_eq!((record.run_id.as_str(), record.state), (run_id.as_str(), RunState::Running));
    }
    let finished = store
        .finish(&run_id, claimed.epoch, RunOutcome::Completed(serde_json::json!("ok")))
        .await
        .expect("a fenced write after dropped calls");
    assert_eq!(finished.state, RunState::Completed);
}
