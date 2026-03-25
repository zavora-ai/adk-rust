//! Bug Condition Exploration Test — Runner String IDs
//!
//! **Property 1 (Bug 3): Runner::run() accepts typed UserId and SessionId**
//!
//! Asserts that `Runner::run()` accepts `UserId` and `SessionId` typed
//! parameters instead of raw `String`. On unfixed code, the signature accepts
//! `String`, so passing typed IDs fails to compile.
//!
//! This test also verifies that `UserId::new()` and `SessionId::new()`
//! validated constructors exist and work correctly.
//!
//! **Validates: Requirements 1.7, 1.8**

use std::sync::Arc;

use adk_core::{Agent, Content, EventStream, InvocationContext, Part, Result, SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{Event, Events, GetRequest, Session, SessionService, State};
use async_trait::async_trait;
use futures::StreamExt;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Mock types (minimal, just enough to construct a Runner)
// ---------------------------------------------------------------------------

struct MockAgent;

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        "test-agent"
    }

    fn description(&self) -> &str {
        "mock"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct MockEvents;

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        vec![]
    }

    fn len(&self) -> usize {
        0
    }

    fn at(&self, _index: usize) -> Option<&Event> {
        None
    }
}

struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

impl State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn set(&mut self, _key: String, _value: serde_json::Value) {}

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

struct MockSession {
    id: String,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        "test-app"
    }

    fn user_id(&self) -> &str {
        "user-1"
    }

    fn state(&self) -> &dyn State {
        &MockState
    }

    fn events(&self) -> &dyn Events {
        &MockEvents
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, _req: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!()
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession { id: req.session_id }))
    }

    async fn list(&self, _req: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }

    async fn delete(&self, _req: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_runner() -> Runner {
    Runner::new(RunnerConfig {
        app_name: "test-app".to_string(),
        agent: Arc::new(MockAgent),
        session_service: Arc::new(MockSessionService),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// Property test
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: production-hardening, Property 1: Bug Condition — Runner Typed IDs**
    ///
    /// *For any* valid user_id and session_id strings, `Runner::run()` SHALL
    /// accept `UserId` and `SessionId` typed parameters (not raw `String`).
    ///
    /// This is primarily a compile-time check — if the signature accepted
    /// `String`, passing `UserId`/`SessionId` would fail to compile.
    ///
    /// **Validates: Requirements 1.7, 1.8**
    #[test]
    fn prop_runner_run_accepts_typed_ids(
        user_id_str in "[a-zA-Z0-9_-]{1,50}",
        session_id_str in "[a-zA-Z0-9_-]{1,50}",
    ) {
        // UserId::new() and SessionId::new() must exist and validate
        let user_id = UserId::new(&user_id_str).expect("valid user_id should parse");
        let session_id = SessionId::new(&session_id_str).expect("valid session_id should parse");

        // Verify round-trip: typed ID preserves the original string
        prop_assert_eq!(user_id.as_ref(), user_id_str.as_str());
        prop_assert_eq!(session_id.as_ref(), session_id_str.as_str());
    }
}

/// Verify Runner::run() compiles with typed IDs and produces a valid stream.
///
/// This is the key compile-time assertion: if Runner::run() accepted String,
/// this code would not compile because UserId is not String.
#[tokio::test]
async fn bug_condition_runner_run_accepts_typed_ids() {
    let runner = make_runner();

    let user_id = UserId::new("user123").unwrap();
    let session_id = SessionId::new("session456").unwrap();
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "Hello".to_string() }] };

    // This call compiles only if Runner::run() accepts UserId and SessionId
    let result = runner.run(user_id, session_id, content).await;
    assert!(result.is_ok(), "Runner::run() with typed IDs should succeed");

    // Drain the stream to verify it works end-to-end
    let mut stream = result.unwrap();
    while let Some(event_result) = stream.next().await {
        assert!(event_result.is_ok());
    }
}

/// Verify that invalid IDs are rejected at the boundary (not inside the stream).
#[test]
fn bug_condition_empty_user_id_rejected_at_boundary() {
    let result = UserId::new("");
    assert!(result.is_err(), "empty UserId should be rejected by new()");
}

#[test]
fn bug_condition_empty_session_id_rejected_at_boundary() {
    let result = SessionId::new("");
    assert!(result.is_err(), "empty SessionId should be rejected by new()");
}
