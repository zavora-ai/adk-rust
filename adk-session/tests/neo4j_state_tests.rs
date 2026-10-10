//! Shared state of the Neo4j session backend against a live server.
//!
//! Run with a server at `NEO4J_URI` (default `bolt://localhost:7687`), authenticating as
//! `NEO4J_USER` (default `neo4j`) with `NEO4J_PASSWORD`; the test uses a fresh app name
//! per run:
//!
//! ```bash
//! NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-session --features neo4j --run-ignored only neo4j_state
//! ```

#![cfg(feature = "neo4j")]

mod common;

use adk_session::Neo4jSessionService;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[tokio::test]
#[ignore = "requires a Neo4j server at NEO4J_URI"]
async fn neo4j_state_shared_tiers_are_read_at_get_time() {
    let service = Neo4jSessionService::new(
        &env_or("NEO4J_URI", "bolt://localhost:7687"),
        &env_or("NEO4J_USER", "neo4j"),
        &env_or("NEO4J_PASSWORD", ""),
    )
    .await
    .expect("connect to neo4j");
    service.migrate().await.expect("migrate session schema");

    common::state_contract::assert_shared_state_contract(
        &service,
        &format!("shared-{}", uuid::Uuid::new_v4()),
        "user1",
        "user2",
    )
    .await;
}
