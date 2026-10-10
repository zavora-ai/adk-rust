//! Shared state and concurrent writes of the Neo4j session backend against a live server.
//!
//! Run with a server at `NEO4J_URI` (default `bolt://localhost:7687`), authenticating as
//! `NEO4J_USER` (default `neo4j`) with `NEO4J_PASSWORD`; each test uses a fresh app name
//! per run. The first run migrates the schema, so run the tests one at a time:
//!
//! ```bash
//! NEO4J_PASSWORD=secret \
//!   cargo nextest run -p adk-session --features neo4j --run-ignored only -j 1 neo4j_state
//! ```

#![cfg(feature = "neo4j")]

mod common;

use adk_session::{CreateRequest, Event, GetRequest, Neo4jSessionService, SessionService};
use serde_json::json;
use std::collections::HashMap;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

async fn service() -> Neo4jSessionService {
    let service = Neo4jSessionService::new(
        &env_or("NEO4J_URI", "bolt://localhost:7687"),
        &env_or("NEO4J_USER", "neo4j"),
        &env_or("NEO4J_PASSWORD", ""),
    )
    .await
    .expect("connect to neo4j");
    service.migrate().await.expect("migrate session schema");
    service
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires a Neo4j server at NEO4J_URI"]
async fn neo4j_state_shared_tiers_are_read_at_get_time() {
    common::state_contract::assert_shared_state_contract(
        &service().await,
        &run_app("shared"),
        "user1",
        "user2",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Neo4j server at NEO4J_URI"]
async fn neo4j_state_concurrent_writes_keep_every_delta() {
    common::state_contract::assert_concurrent_state_writes(
        &service().await,
        &run_app("concurrent"),
        "user1",
        12,
    )
    .await;
}

/// Earlier releases kept each tier as one JSON object in the node's `state` property.
#[tokio::test]
#[ignore = "requires a Neo4j server at NEO4J_URI"]
async fn neo4j_state_keeps_tiers_written_by_earlier_releases() {
    let service = service().await;
    let app_name = run_app("legacy");
    let session = service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: "user1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .expect("create session");
    service
        .graph()
        .run(
            neo4rs::query("MATCH (a:AppState {app_name: $app_name}) SET a.state = $state")
                .param("app_name", app_name.clone())
                .param("state", r#"{"theme":"light","plan":"free"}"#),
        )
        .await
        .expect("write a legacy app tier");

    let mut event = Event::new("inv-legacy");
    event.actions.state_delta.insert("app:theme".to_string(), json!("dark"));
    service.append_event(session.id(), event).await.expect("append event");

    let fetched = service
        .get(GetRequest {
            app_name,
            user_id: "user1".to_string(),
            session_id: session.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get session");
    let expected: HashMap<String, serde_json::Value> =
        [("app:theme".to_string(), json!("dark")), ("app:plan".to_string(), json!("free"))].into();
    assert_eq!(fetched.state().all(), expected);
}
