//! Integration tests for adk-auth RBAC functionality.

use adk_auth::{AccessControl, AuthMiddleware, Permission, Role, ToolExt};
use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool,
    ToolContext,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Mock tool for testing
struct MockTool {
    name: String,
}

impl MockTool {
    fn new(name: &str) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "A mock tool for testing"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({
            "tool": self.name,
            "args": args,
            "status": "executed"
        }))
    }
}

/// Mock context for testing
struct MockContext {
    user_id: String,
    content: Content,
    actions: Mutex<EventActions>,
}

impl MockContext {
    fn create(user_id: &str) -> Arc<dyn ToolContext> {
        Arc::new(Self {
            user_id: user_id.into(),
            content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "test-invocation"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for MockContext {
    fn function_call_id(&self) -> &str {
        "test-call-id"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
}

// =============================================================================
// AccessControl Tests
// =============================================================================

#[test]
fn test_basic_role_access() {
    let admin = Role::new("admin").allow(Permission::AllTools);
    let user = Role::new("user").allow(Permission::Tool("search".into()));

    let ac = AccessControl::builder()
        .role(admin)
        .role(user)
        .assign("alice", "admin")
        .assign("bob", "user")
        .build()
        .unwrap();

    // Admin can access everything
    assert!(ac.check("alice", &Permission::Tool("search".into())).is_ok());
    assert!(ac.check("alice", &Permission::Tool("admin".into())).is_ok());
    assert!(ac.check("alice", &Permission::AllTools).is_ok());

    // User can only access search
    assert!(ac.check("bob", &Permission::Tool("search".into())).is_ok());
    assert!(ac.check("bob", &Permission::Tool("admin".into())).is_err());
    assert!(ac.check("bob", &Permission::AllTools).is_err());
}

#[test]
fn test_deny_precedence() {
    let role =
        Role::new("limited").allow(Permission::AllTools).deny(Permission::Tool("dangerous".into()));

    let ac = AccessControl::builder().role(role).assign("user", "limited").build().unwrap();

    // Can access other tools
    assert!(ac.check("user", &Permission::Tool("safe".into())).is_ok());

    // Cannot access denied tool
    assert!(ac.check("user", &Permission::Tool("dangerous".into())).is_err());
}

#[test]
fn test_multi_role_union() {
    let reader = Role::new("reader").allow(Permission::Tool("read".into()));
    let writer = Role::new("writer").allow(Permission::Tool("write".into()));

    let ac = AccessControl::builder()
        .role(reader)
        .role(writer)
        .assign("alice", "reader")
        .assign("alice", "writer")
        .build()
        .unwrap();

    // Alice has both permissions
    assert!(ac.check("alice", &Permission::Tool("read".into())).is_ok());
    assert!(ac.check("alice", &Permission::Tool("write".into())).is_ok());
}

#[test]
fn test_unknown_user_denied() {
    let role = Role::new("user").allow(Permission::AllTools);

    let ac = AccessControl::builder().role(role).assign("known", "user").build().unwrap();

    assert!(ac.check("known", &Permission::Tool("any".into())).is_ok());
    assert!(ac.check("unknown", &Permission::Tool("any".into())).is_err());
}

#[test]
fn test_invalid_role_assignment_fails() {
    let result =
        AccessControl::builder().role(Role::new("admin")).assign("alice", "nonexistent").build();

    assert!(result.is_err());
}

// =============================================================================
// ProtectedTool Tests
// =============================================================================

#[tokio::test]
async fn test_protected_tool_allows_authorized() {
    let role = Role::new("user").allow(Permission::Tool("search".into()));

    let ac = AccessControl::builder().role(role).assign("alice", "user").build().unwrap();

    let tool = MockTool::new("search");
    let protected = tool.with_access_control(Arc::new(ac));

    let ctx = MockContext::create("alice");
    let result = protected.execute(ctx, json!({"query": "test"})).await;

    assert!(result.is_ok());
    let value = result.unwrap();
    assert_eq!(value["status"], "executed");
}

#[tokio::test]
async fn test_protected_tool_denies_unauthorized() {
    let role = Role::new("user").allow(Permission::Tool("other".into()));

    let ac = AccessControl::builder().role(role).assign("alice", "user").build().unwrap();

    let tool = MockTool::new("search");
    let protected = tool.with_access_control(Arc::new(ac));

    let ctx = MockContext::create("alice");
    let result = protected.execute(ctx, json!({})).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn a_protected_tool_denial_is_a_structured_permission_error() {
    // spec 3.1 §7.14: the denial names the permission and is not an internal tool failure.
    let role = Role::new("user").allow(Permission::Tool("other".into()));
    let ac = AccessControl::builder().role(role).assign("alice", "user").build().unwrap();
    let protected = MockTool::new("search").with_access_control(Arc::new(ac));

    let error = protected.execute(MockContext::create("alice"), json!({})).await.unwrap_err();

    assert_eq!(
        (error.component, error.category, error.code),
        (
            adk_core::ErrorComponent::Tool,
            adk_core::ErrorCategory::Forbidden,
            adk_core::TOOL_PERMISSION_DENIED_CODE
        )
    );
    assert_eq!(error.details.metadata["permission"], json!("tool:search"));
    let response = adk_core::tool_error_response("search", &error, false);
    assert!(
        response["error"].as_str().unwrap().contains("cannot access tool:search"),
        "the model is told which permission was missing: {response}"
    );
}

// =============================================================================
// AuthMiddleware Tests
// =============================================================================

#[test]
fn test_middleware_protects_all_tools() {
    let admin = Role::new("admin").allow(Permission::AllTools);

    let ac = AccessControl::builder().role(admin).assign("admin", "admin").build().unwrap();

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(MockTool::new("tool1")),
        Arc::new(MockTool::new("tool2")),
        Arc::new(MockTool::new("tool3")),
    ];

    let middleware = AuthMiddleware::new(ac);
    let protected = middleware.protect_all(tools);

    assert_eq!(protected.len(), 3);
    assert_eq!(protected[0].name(), "tool1");
    assert_eq!(protected[1].name(), "tool2");
    assert_eq!(protected[2].name(), "tool3");
}

#[tokio::test]
async fn test_middleware_batch_execution() {
    let role = Role::new("user")
        .allow(Permission::Tool("allowed1".into()))
        .allow(Permission::Tool("allowed2".into()));

    let ac = AccessControl::builder().role(role).assign("user", "user").build().unwrap();

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(MockTool::new("allowed1")),
        Arc::new(MockTool::new("allowed2")),
        Arc::new(MockTool::new("denied")),
    ];

    let middleware = AuthMiddleware::new(ac);
    let protected = middleware.protect_all(tools);

    let ctx = MockContext::create("user");

    // Allowed tools work
    assert!(protected[0].execute(ctx.clone(), json!({})).await.is_ok());
    assert!(protected[1].execute(ctx.clone(), json!({})).await.is_ok());

    // Denied tool fails
    assert!(protected[2].execute(ctx, json!({})).await.is_err());
}

// =============================================================================
// Permission Matching Tests
// =============================================================================

#[test]
fn test_permission_wildcards() {
    let role = Role::new("all_tools").allow(Permission::AllTools);

    let ac = AccessControl::builder().role(role).assign("user", "all_tools").build().unwrap();

    // AllTools covers any specific tool
    assert!(ac.check("user", &Permission::Tool("tool1".into())).is_ok());
    assert!(ac.check("user", &Permission::Tool("tool2".into())).is_ok());
    assert!(ac.check("user", &Permission::AllTools).is_ok());
}

#[test]
fn test_agent_permissions() {
    let role = Role::new("agent_access")
        .allow(Permission::Agent("agent1".into()))
        .allow(Permission::AllAgents);

    let ac = AccessControl::builder().role(role).assign("user", "agent_access").build().unwrap();

    assert!(ac.check("user", &Permission::Agent("agent1".into())).is_ok());
    assert!(ac.check("user", &Permission::Agent("any".into())).is_ok());
    assert!(ac.check("user", &Permission::AllAgents).is_ok());
}
