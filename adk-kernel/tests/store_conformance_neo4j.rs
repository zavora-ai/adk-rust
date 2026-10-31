//! The kernel's Neo4j stores (spec 3.2 §7.1, §7.4, §11, §13 Q8): the process, agenda, driver,
//! ledger, partition and cron watermark conformance suites against a live server, replicas
//! migrating together, and the stores sharing one connection with adk-session's run, schedule
//! and governance stores.
//!
//! The tests are `#[ignore]`d because they need a server; CI's nightly `durable-stores` job
//! runs them. Run with a server at `NEO4J_URI` (default `bolt://localhost:7687`),
//! authenticating as `NEO4J_USER` (default `neo4j`) with `NEO4J_PASSWORD`. Every case writes
//! under fresh identifiers, so the cases share one database, and every `open` is a new
//! connection, as a restarted or second replica would create:
//!
//! ```bash
//! NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-kernel --features neo4j --run-ignored only \
//!   --test integration -E 'test(/^store_conformance_neo4j::/)'
//! ```

#![cfg(feature = "neo4j")]

use std::sync::Arc;

use adk_core::{DurableRunStore, ScheduleStore};
use adk_kernel::conformance::{AgendaStoreHarness, ProcessStoreHarness};
use adk_kernel::{
    AgendaStore, Neo4jAgendaStore, Neo4jProcessStore, ProcessKind, ProcessMeta, ProcessStore,
    QueueClass,
};
use neo4rs::Graph;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// A new connection per call, as a restarted or second replica would open.
async fn graph() -> Graph {
    Graph::new(
        env_or("NEO4J_URI", "bolt://localhost:7687"),
        env_or("NEO4J_USER", "neo4j"),
        env_or("NEO4J_PASSWORD", ""),
    )
    .await
    .expect("connect to neo4j")
}

struct Neo4j;

#[async_trait::async_trait]
impl ProcessStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        let store = Neo4jProcessStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl AgendaStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        let store = Neo4jAgendaStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "drivers")]
#[async_trait::async_trait]
impl adk_kernel::conformance::DriverStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn adk_kernel::drivers::DriverStore> {
        let store = adk_kernel::stores::Neo4jDriverStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "proc")]
#[async_trait::async_trait]
impl adk_kernel::conformance::LedgerStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn adk_kernel::proc::LedgerStore> {
        let store = adk_kernel::proc::Neo4jLedgerStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl adk_kernel::conformance::PartitionStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn adk_kernel::PartitionStore> {
        let store = adk_kernel::Neo4jPartitionStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl adk_kernel::conformance::CronWatermarkStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn adk_kernel::CronWatermarkStore> {
        let store = adk_kernel::Neo4jCronWatermarkStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod process {
    use super::*;

    adk_kernel::process_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

mod agenda {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

#[cfg(feature = "drivers")]
mod drivers {
    use super::*;

    adk_kernel::driver_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

#[cfg(feature = "proc")]
mod ledger {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

mod partition {
    use super::*;

    adk_kernel::partition_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

mod watermark {
    use super::*;

    adk_kernel::cron_watermark_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}

/// Replicas starting together create the schema once: every `migrate` succeeds.
#[tokio::test]
#[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
async fn replicas_starting_together_migrate() {
    let mut starts = Vec::new();
    for _ in 0..4 {
        starts.push(tokio::spawn(async move {
            let graph = graph().await;
            Neo4jProcessStore::from_graph(graph.clone()).migrate().await?;
            Neo4jAgendaStore::from_graph(graph).migrate().await
        }));
    }
    for start in starts {
        start.await.expect("start completes").expect("migrate");
    }
}

/// One connection serves the kernel's stores beside adk-session's run and schedule stores.
#[tokio::test]
#[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
async fn one_connection_serves_the_kernel_and_run_stores() {
    let graph = graph().await;
    let runs = adk_session::Neo4jRunStore::from_graph(graph.clone());
    let schedules = adk_session::Neo4jScheduleStore::from_graph(graph.clone());
    let processes = Neo4jProcessStore::from_graph(graph);
    runs.migrate().await.expect("migrate the run store");
    schedules.migrate().await.expect("migrate the schedule store");
    processes.migrate().await.expect("migrate the process store");
    let pid = format!("run-{}", uuid::Uuid::new_v4());
    let meta = ProcessMeta::new(pid.as_str(), QueueClass::Background, ProcessKind::Goal);
    processes.put(meta.clone()).await.expect("put");
    assert_eq!(processes.get(&pid).await.expect("get"), Some(meta));
    assert!(runs.get(&pid).await.expect("read the run store").is_none());
    schedules.list().await.expect("list schedules");
    assert!(processes.delete(&pid).await.expect("delete"));
}

// The agent store (spec 3.3 §10 K7).
#[async_trait::async_trait]
impl adk_kernel::conformance::AgentStoreHarness for Neo4j {
    async fn open(&self) -> Arc<dyn adk_kernel::agents::AgentStore> {
        let store = adk_kernel::stores::Neo4jAgentStore::from_graph(graph().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod agents {
    use super::*;

    adk_kernel::agent_store_conformance_tests! {
        #[ignore = "needs NEO4J_URI and NEO4J_PASSWORD; CI's nightly durable-stores job runs it"]
        harness = Neo4j
    }
}
