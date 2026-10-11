//! The Neo4j checkpointer passes the shared conformance suite.
//!
//! Run with a server at `NEO4J_URI` (default `bolt://localhost:7687`), authenticating as
//! `NEO4J_USER` (default `neo4j`) with `NEO4J_PASSWORD`. Every case writes under fresh
//! identifiers, so the cases share one database:
//!
//! ```bash
//! NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-graph --features neo4j --run-ignored only \
//!   --test checkpointer_conformance_neo4j
//! ```

#![cfg(feature = "neo4j")]

use std::sync::Arc;

use adk_graph::Neo4jCheckpointer;
use adk_graph::checkpoint::Checkpointer;
use adk_graph::conformance::CheckpointerHarness;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// A new connection per call, as a restarted or second process would open.
struct Neo4j;

#[async_trait::async_trait]
impl CheckpointerHarness for Neo4j {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let checkpointer = Neo4jCheckpointer::new(
            &env_or("NEO4J_URI", "bolt://localhost:7687"),
            &env_or("NEO4J_USER", "neo4j"),
            &env_or("NEO4J_PASSWORD", ""),
        )
        .await
        .expect("connect to neo4j");
        checkpointer.migrate().await.expect("migrate checkpoint schema");
        Arc::new(checkpointer)
    }
}

adk_graph::checkpointer_conformance_tests!(
    #[ignore = "requires a Neo4j server at NEO4J_URI"]
    harness = Neo4j,
);
