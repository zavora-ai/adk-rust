//! Shared state, concurrent writes, and event filters of the Redis session backend
//! against a live server.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`); the tests use a
//! fresh app name per run:
//!
//! ```bash
//! cargo nextest run -p adk-session --features redis --run-ignored only redis_state
//! ```

#![cfg(feature = "redis")]

mod common;

use adk_session::{RedisSessionConfig, RedisSessionService};

async fn service() -> RedisSessionService {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    RedisSessionService::new(RedisSessionConfig { url, ttl: None, cluster_nodes: None })
        .await
        .expect("connect to redis")
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_state_shared_tiers_are_read_at_get_time() {
    let service = service().await;
    common::state_contract::assert_shared_state_contract(
        &service,
        &run_app("shared"),
        "user1",
        "user2",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_state_concurrent_writes_keep_every_delta() {
    let service = service().await;
    common::state_contract::assert_concurrent_state_writes(
        &service,
        &run_app("concurrent"),
        "user1",
        12,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_state_event_filters() {
    let service = service().await;
    common::state_contract::assert_event_filter_contract(&service, &run_app("filters"), "user1")
        .await;
}
