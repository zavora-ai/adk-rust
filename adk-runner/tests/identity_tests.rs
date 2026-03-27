//! Regression tests for typed identity projection from `InvocationContext`.
//!
//! **Feature: typed-identity, Property 5: Execution Identity Projection**
//!
//! For any valid `ReadonlyContext`, `try_identity()` returns the stable session
//! triple and `try_execution_identity()` returns the same session triple plus
//! the invocation, branch, and agent data from the context.
//!
//! **Validates: Requirements 4.1, 6.1, 6.2, 8.3, 11.1**

use adk_core::{
    Agent, Content, InvocationContext as InvocationContextTrait, Part, ReadonlyContext,
    RequestContext,
};
use adk_runner::InvocationContext;
use adk_session::{Events, Session, State};
use async_trait::async_trait;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Test helpers (mirrors context_tests.rs patterns)
// ---------------------------------------------------------------------------

struct MockEvents;
impl Events for MockEvents {
    fn all(&self) -> Vec<adk_core::Event> {
        Vec::new()
    }
    fn len(&self) -> usize {
        0
    }
    fn at(&self, _index: usize) -> Option<&adk_core::Event> {
        None
    }
}

struct MockStateView;
impl State for MockStateView {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-abc"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-123"
    }
    fn state(&self) -> &dyn State {
        static VIEW: MockStateView = MockStateView;
        &VIEW
    }
    fn events(&self) -> &dyn Events {
        static EVENTS: MockEvents = MockEvents;
        &EVENTS
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

struct MockAgent {
    name: String,
}

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "mock"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(
        &self,
        _ctx: Arc<dyn InvocationContextTrait>,
    ) -> adk_core::Result<adk_core::EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Helper to build an `InvocationContext` with the given identity fields.
fn make_ctx(
    app: &str,
    user: &str,
    session: &str,
    invocation: &str,
    agent: &str,
    branch: &str,
) -> InvocationContext {
    let agent = Arc::new(MockAgent { name: agent.to_string() });
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "hi".to_string() }] };
    let ctx = InvocationContext::new(
        invocation.to_string(),
        agent,
        user.to_string(),
        app.to_string(),
        session.to_string(),
        content,
        Arc::new(MockSession),
    )
    .expect("test identity values must be valid");
    if branch.is_empty() { ctx } else { ctx.with_branch(branch.to_string()) }
}

// ---------------------------------------------------------------------------
// Unit tests: try_identity()
// ---------------------------------------------------------------------------

#[test]
fn try_identity_returns_correct_session_triple() {
    let ctx = make_ctx("my-app", "alice", "sess-1", "inv-1", "planner", "");
    let identity = ctx.try_identity().expect("try_identity should succeed");

    assert_eq!(identity.app_name.as_ref(), "my-app");
    assert_eq!(identity.user_id.as_ref(), "alice");
    assert_eq!(identity.session_id.as_ref(), "sess-1");
}

#[test]
fn try_identity_with_special_chars() {
    let ctx = make_ctx(
        "org:weather-app",
        "tenant:alice@example.com",
        "sess/2024/abc",
        "inv-1",
        "agent",
        "",
    );
    let identity = ctx.try_identity().expect("special chars should be accepted");

    assert_eq!(identity.app_name.as_ref(), "org:weather-app");
    assert_eq!(identity.user_id.as_ref(), "tenant:alice@example.com");
    assert_eq!(identity.session_id.as_ref(), "sess/2024/abc");
}

// ---------------------------------------------------------------------------
// Unit tests: try_execution_identity()
// ---------------------------------------------------------------------------

#[test]
fn try_execution_identity_returns_full_capsule() {
    let ctx = make_ctx("my-app", "alice", "sess-1", "inv-42", "planner", "main.sub");
    let exec = ctx.try_execution_identity().expect("should succeed");

    assert_eq!(exec.adk.app_name.as_ref(), "my-app");
    assert_eq!(exec.adk.user_id.as_ref(), "alice");
    assert_eq!(exec.adk.session_id.as_ref(), "sess-1");
    assert_eq!(exec.invocation_id.as_ref(), "inv-42");
    assert_eq!(exec.branch, "main.sub");
    assert_eq!(exec.agent_name, "planner");
}

#[test]
fn try_execution_identity_default_branch_is_empty() {
    let ctx = make_ctx("app", "user", "sess", "inv", "agent", "");
    let exec = ctx.try_execution_identity().unwrap();
    assert_eq!(exec.branch, "");
}

// ---------------------------------------------------------------------------
// Unit tests: session triple shared between try_identity and try_execution_identity
// ---------------------------------------------------------------------------

#[test]
fn identity_and_execution_identity_share_session_triple() {
    let ctx = make_ctx("app-x", "user-y", "sess-z", "inv-w", "agent-v", "branch-u");

    let identity = ctx.try_identity().unwrap();
    let exec = ctx.try_execution_identity().unwrap();

    assert_eq!(identity, exec.adk);
}

// ---------------------------------------------------------------------------
// Unit tests: authenticated user override via RequestContext
// ---------------------------------------------------------------------------

#[test]
fn request_context_overrides_user_id() {
    let ctx = make_ctx("app", "original-user", "sess", "inv", "agent", "").with_request_context(
        RequestContext {
            user_id: "auth-user-override".to_string(),
            scopes: vec!["read".to_string()],
            metadata: HashMap::new(),
        },
    );

    // The string-returning method should reflect the override
    assert_eq!(ctx.user_id(), "auth-user-override");
}

#[test]
fn try_identity_uses_auth_user_when_request_context_set() {
    let ctx = make_ctx("app", "original-user", "sess", "inv", "agent", "").with_request_context(
        RequestContext {
            user_id: "auth-user".to_string(),
            scopes: vec![],
            metadata: HashMap::new(),
        },
    );

    let identity = ctx.try_identity().unwrap();

    // try_identity delegates to user_id() which returns the auth user
    assert_eq!(identity.user_id.as_ref(), "auth-user");
    // app_name and session_id are unchanged
    assert_eq!(identity.app_name.as_ref(), "app");
    assert_eq!(identity.session_id.as_ref(), "sess");
}

#[test]
fn try_execution_identity_uses_auth_user_when_request_context_set() {
    let ctx = make_ctx("app", "original-user", "sess", "inv", "agent", "main")
        .with_request_context(RequestContext {
            user_id: "auth-user".to_string(),
            scopes: vec!["admin".to_string()],
            metadata: HashMap::new(),
        });

    let exec = ctx.try_execution_identity().unwrap();

    // The session triple should use the auth user
    assert_eq!(exec.adk.user_id.as_ref(), "auth-user");
    assert_eq!(exec.adk.app_name.as_ref(), "app");
    assert_eq!(exec.adk.session_id.as_ref(), "sess");
    // Execution fields are unaffected by request context
    assert_eq!(exec.invocation_id.as_ref(), "inv");
    assert_eq!(exec.branch, "main");
    assert_eq!(exec.agent_name, "agent");
}

#[test]
fn without_request_context_user_id_is_original() {
    let ctx = make_ctx("app", "original-user", "sess", "inv", "agent", "");

    assert_eq!(ctx.user_id(), "original-user");
    let identity = ctx.try_identity().unwrap();
    assert_eq!(identity.user_id.as_ref(), "original-user");
}

// ---------------------------------------------------------------------------
// Property 5: Execution Identity Projection (proptest)
// ---------------------------------------------------------------------------

/// Generate a valid identifier string for property tests.
fn arb_valid_id() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9:@/_\\-\\.]{1,64}"
}

/// Generate a branch string (may be empty).
fn arb_branch() -> impl Strategy<Value = String> {
    "[a-z\\.]{0,20}"
}

/// Generate an agent name.
fn arb_agent_name() -> impl Strategy<Value = String> {
    "[a-z_]{1,20}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Property 5: Execution Identity Projection**
    /// *For any* valid ReadonlyContext, `try_identity()` returns the stable
    /// session triple and `try_execution_identity()` returns the same session
    /// triple plus the invocation, branch, and agent data from the context.
    /// **Validates: Requirements 4.1, 6.1, 6.2, 11.1**
    #[test]
    fn prop_execution_identity_projection(
        app in arb_valid_id(),
        user in arb_valid_id(),
        session in arb_valid_id(),
        invocation in arb_valid_id(),
        agent in arb_agent_name(),
        branch in arb_branch(),
    ) {
        let ctx = make_ctx(&app, &user, &session, &invocation, &agent, &branch);

        let identity = ctx.try_identity().expect("try_identity must succeed for valid inputs");
        let exec = ctx.try_execution_identity().expect("try_execution_identity must succeed");

        // Session triple matches the context's string-returning methods
        prop_assert_eq!(identity.app_name.as_ref(), ctx.app_name());
        prop_assert_eq!(identity.user_id.as_ref(), ctx.user_id());
        prop_assert_eq!(identity.session_id.as_ref(), ctx.session_id());

        // Execution identity contains the same session triple
        prop_assert_eq!(&identity, &exec.adk);

        // Execution-scoped fields match the context
        prop_assert_eq!(exec.invocation_id.as_ref(), ctx.invocation_id());
        prop_assert_eq!(exec.branch.as_str(), ctx.branch());
        prop_assert_eq!(exec.agent_name.as_str(), ctx.agent_name());
    }

    /// **Feature: typed-identity, Property 5: Execution Identity Projection**
    /// *For any* valid context with a RequestContext override, `try_identity()`
    /// and `try_execution_identity()` both use the authenticated user, and the
    /// session triple is consistent between the two.
    /// **Validates: Requirements 4.1, 6.1, 8.3, 11.1**
    #[test]
    fn prop_auth_override_consistent_projection(
        app in arb_valid_id(),
        original_user in arb_valid_id(),
        auth_user in arb_valid_id(),
        session in arb_valid_id(),
        invocation in arb_valid_id(),
        agent in arb_agent_name(),
        branch in arb_branch(),
    ) {
        let ctx = make_ctx(&app, &original_user, &session, &invocation, &agent, &branch)
            .with_request_context(RequestContext {
                user_id: auth_user.clone(),
                scopes: vec![],
                metadata: HashMap::new(),
            });

        let identity = ctx.try_identity().expect("try_identity must succeed");
        let exec = ctx.try_execution_identity().expect("try_execution_identity must succeed");

        // Both should use the auth user, not the original
        prop_assert_eq!(identity.user_id.as_ref(), auth_user.as_str());
        prop_assert_eq!(exec.adk.user_id.as_ref(), auth_user.as_str());

        // Session triple is consistent between the two
        prop_assert_eq!(&identity, &exec.adk);

        // app_name and session_id are unchanged
        prop_assert_eq!(identity.app_name.as_ref(), app.as_str());
        prop_assert_eq!(identity.session_id.as_ref(), session.as_str());
    }
}
