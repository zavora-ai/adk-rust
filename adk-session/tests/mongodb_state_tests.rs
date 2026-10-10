//! Shared state and concurrent writes of the MongoDB session backend against a live
//! server.
//!
//! Run with a server at `MONGODB_URL` (default `mongodb://localhost:27017`); the tests
//! use the `adk_session_tests` database and a fresh app name per run:
//!
//! ```bash
//! MONGODB_URL=mongodb://localhost:27017 \
//!   cargo nextest run -p adk-session --features mongodb --run-ignored only mongodb_state
//! ```
//!
//! The concurrent-write test needs a standalone server. On a replica set every append
//! runs in a transaction, and concurrent transactions on one document abort with a
//! write conflict instead of waiting.

#![cfg(feature = "mongodb")]

mod common;

use adk_session::MongoSessionService;

async fn service() -> MongoSessionService {
    let url =
        std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
    let service =
        MongoSessionService::new(&url, "adk_session_tests").await.expect("connect to mongodb");
    service.migrate().await.expect("migrate session collections");
    service
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires a MongoDB server at MONGODB_URL"]
async fn mongodb_state_shared_tiers_are_read_at_get_time() {
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
#[ignore = "requires a standalone MongoDB server at MONGODB_URL"]
async fn mongodb_state_concurrent_writes_keep_every_delta() {
    let service = service().await;
    assert!(
        !service.supports_transactions(),
        "this test needs a standalone server; see the module documentation"
    );
    common::state_contract::assert_concurrent_state_writes(
        &service,
        &run_app("concurrent"),
        "user1",
        12,
    )
    .await;
}
