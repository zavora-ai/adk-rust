//! Integration tests for adk-auth RBAC functionality.

use adk_auth::{AccessControl, AuthMiddleware, Permission, Role, ToolExt};
use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool,
    ToolContext,
    types::{InvocationId, SessionId, UserId},
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
    identity: adk_core::types::AdkIdentity,
    content: Content,
    actions: Mutex<EventActions>,
}

impl MockContext {
    fn create(user_id: &UserId) -> Arc<dyn ToolContext> {
        let mut identity = adk_core::types::AdkIdentity::default();
        identity.invocation_id = InvocationId::new("test-invocation").unwrap();
        identity.agent_name = "test-agent".to_string();
        identity.user_id = user_id.clone();
        identity.app_name = "test-app".to_string();
        identity.session_id = SessionId::new("test-session").unwrap();

        Arc::new(Self {
            identity,
            content: Content::user(),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.content
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        static METADATA: std::sync::OnceLock<std::collections::HashMap<String, String>> =
            std::sync::OnceLock::new();
        METADATA.get_or_init(std::collections::HashMap::new)
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
        .assign(UserId::new("alice").unwrap(), "admin")
        .assign(UserId::new("bob").unwrap(), "user")
        .build()
        .unwrap();

    // Admin can access everything
    assert!(ac.check(&UserId::new("alice").unwrap(), &Permission::Tool("search".into())).is_ok());
    assert!(ac.check(&UserId::new("alice").unwrap(), &Permission::Tool("admin".into())).is_ok());
    assert!(ac.check(&UserId::new("alice").unwrap(), &Permission::AllTools).is_ok());

    // User can only access search
    assert!(ac.check(&UserId::new("bob").unwrap(), &Permission::Tool("search".into())).is_ok());
    assert!(ac.check(&UserId::new("bob").unwrap(), &Permission::Tool("admin".into())).is_err());
    assert!(ac.check(&UserId::new("bob").unwrap(), &Permission::AllTools).is_err());
}

#[test]
fn test_deny_precedence() {
    let role =
        Role::new("limited").allow(Permission::AllTools).deny(Permission::Tool("dangerous".into()));

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("user").unwrap(), "limited")
        .build()
        .unwrap();

    // Can access other tools
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::Tool("safe".into())).is_ok());

    // Cannot access denied tool
    assert!(
        ac.check(&UserId::new("user").unwrap(), &Permission::Tool("dangerous".into())).is_err()
    );
}

#[test]
fn test_multi_role_union() {
    let reader = Role::new("reader").allow(Permission::Tool("read".into()));
    let writer = Role::new("writer").allow(Permission::Tool("write".into()));

    let ac = AccessControl::builder()
        .role(reader)
        .role(writer)
        .assign(UserId::new("alice").unwrap(), "reader")
        .assign(UserId::new("alice").unwrap(), "writer")
        .build()
        .unwrap();

    // Alice has both permissions
    assert!(ac.check(&UserId::new("alice").unwrap(), &Permission::Tool("read".into())).is_ok());
    assert!(ac.check(&UserId::new("alice").unwrap(), &Permission::Tool("write".into())).is_ok());
}

#[test]
fn test_unknown_user_denied() {
    let role = Role::new("user").allow(Permission::AllTools);

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("known").unwrap(), "user")
        .build()
        .unwrap();

    assert!(ac.check(&UserId::new("known").unwrap(), &Permission::Tool("any".into())).is_ok());
    assert!(ac.check(&UserId::new("unknown").unwrap(), &Permission::Tool("any".into())).is_err());
}

#[test]
fn test_invalid_role_assignment_fails() {
    let result = AccessControl::builder()
        .role(Role::new("admin"))
        .assign(UserId::new("alice").unwrap(), "nonexistent")
        .build();

    assert!(result.is_err());
}

// =============================================================================
// ProtectedTool Tests
// =============================================================================

#[tokio::test]
async fn test_protected_tool_allows_authorized() {
    let role = Role::new("user").allow(Permission::Tool("search".into()));

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("alice").unwrap(), "user")
        .build()
        .unwrap();

    let tool = MockTool::new("search");
    let protected = tool.with_access_control(Arc::new(ac));

    let ctx = MockContext::create(&UserId::new("alice").unwrap());
    let result = protected.execute(ctx.clone(), json!({"query": "test"})).await;

    assert!(result.is_ok());
    let value = result.unwrap();
    assert_eq!(value["status"], "executed");
}

#[tokio::test]
async fn test_protected_tool_denies_unauthorized() {
    let role = Role::new("user").allow(Permission::Tool("other".into()));

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("alice").unwrap(), "user")
        .build()
        .unwrap();

    let tool = MockTool::new("search");
    let protected = tool.with_access_control(Arc::new(ac));

    let ctx = MockContext::create(&UserId::new("alice").unwrap());
    let result = protected.execute(ctx, json!({})).await;

    assert!(result.is_err());
}

// =============================================================================
// AuthMiddleware Tests
// =============================================================================

#[test]
fn test_middleware_protects_all_tools() {
    let admin = Role::new("admin").allow(Permission::AllTools);

    let ac = AccessControl::builder()
        .role(admin)
        .assign(UserId::new("admin").unwrap(), "admin")
        .build()
        .unwrap();

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

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("user").unwrap(), "user")
        .build()
        .unwrap();

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(MockTool::new("allowed1")),
        Arc::new(MockTool::new("allowed2")),
        Arc::new(MockTool::new("denied")),
    ];

    let middleware = AuthMiddleware::new(ac);
    let protected = middleware.protect_all(tools);

    let ctx = MockContext::create(&UserId::new("user").unwrap());

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

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("user").unwrap(), "all_tools")
        .build()
        .unwrap();

    // AllTools covers any specific tool
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::Tool("tool1".into())).is_ok());
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::Tool("tool2".into())).is_ok());
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::AllTools).is_ok());
}

#[test]
fn test_agent_permissions() {
    let role = Role::new("agent_access")
        .allow(Permission::Agent("agent1".into()))
        .allow(Permission::AllAgents);

    let ac = AccessControl::builder()
        .role(role)
        .assign(UserId::new("user").unwrap(), "agent_access")
        .build()
        .unwrap();

    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::Agent("agent1".into())).is_ok());
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::Agent("any".into())).is_ok());
    assert!(ac.check(&UserId::new("user").unwrap(), &Permission::AllAgents).is_ok());
}
