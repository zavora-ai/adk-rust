//! Integration tests for the declarative scope-based security system.
//!
//! Demonstrates the "Security State Machine" pattern where tools declare
//! their required scopes and the framework enforces them automatically —
//! no imperative checks inside tool handlers.

use adk_auth::{
    ContextScopeResolver, ScopeDenied, ScopeGuard, ScopeResolver, ScopeToolExt,
    StaticScopeResolver, check_scopes,
};
use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool,
    ToolContext,
    types::{InvocationId, SessionId, UserId},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

// =============================================================================
// Mock tools — note: NO imperative security checks inside execute()
// =============================================================================

/// A tool that requires finance:write and verified scopes.
struct TransferFundsTool;

#[async_trait]
impl Tool for TransferFundsTool {
    fn name(&self) -> &str {
        "transfer_funds"
    }

    fn description(&self) -> &str {
        "Transfer funds between accounts"
    }

    fn required_scopes(&self) -> &[&str] {
        &["finance:write", "verified"]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        // No security checks here — the framework handles it
        Ok(json!({
            "status": "transferred",
            "amount": args["amount"],
        }))
    }
}

/// A tool that requires admin scope.
struct AdminPanelTool;

#[async_trait]
impl Tool for AdminPanelTool {
    fn name(&self) -> &str {
        "admin_panel"
    }

    fn description(&self) -> &str {
        "Access admin panel"
    }

    fn required_scopes(&self) -> &[&str] {
        &["admin"]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "status": "admin_access_granted" }))
    }
}

/// A tool with no scope requirements — open to everyone.
struct PublicSearchTool;

#[async_trait]
impl Tool for PublicSearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Public search tool"
    }

    // No required_scopes override — defaults to &[] (open)

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "results": [], "query": args["query"] }))
    }
}

// =============================================================================
// Mock context with configurable scopes
// =============================================================================

struct ScopedMockContext {
    identity: adk_core::types::AdkIdentity,
    scopes: Vec<String>,
    content: Content,
    actions: Mutex<EventActions>,
}

impl ScopedMockContext {
    fn create(user_id: &UserId, scopes: Vec<&str>) -> Arc<dyn ToolContext> {
        let mut identity = adk_core::types::AdkIdentity::default();
        identity.invocation_id = InvocationId::new("test-invocation").unwrap();
        identity.agent_name = "test-agent".to_string();
        identity.user_id = user_id.clone();
        identity.app_name = "test-app".to_string();
        identity.session_id = SessionId::new("test-session").unwrap();

        Arc::new(Self {
            identity,
            scopes: scopes.into_iter().map(String::from).collect(),
            content: Content::user(),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for ScopedMockContext {
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
impl CallbackContext for ScopedMockContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for ScopedMockContext {
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
    fn user_scopes(&self) -> Vec<String> {
        self.scopes.clone()
    }
}

// =============================================================================
// check_scopes() unit tests
// =============================================================================

#[test]
fn test_check_scopes_no_requirements() {
    // Tools with no scope requirements always pass
    assert!(check_scopes(&[], &[]).is_ok());
    assert!(check_scopes(&[], &["admin".to_string()]).is_ok());
}

#[test]
fn test_check_scopes_all_satisfied() {
    let granted =
        vec!["finance:read".to_string(), "finance:write".to_string(), "verified".to_string()];
    assert!(check_scopes(&["finance:write", "verified"], &granted).is_ok());
}

#[test]
fn test_check_scopes_superset_granted() {
    // User has more scopes than required — should pass
    let granted = vec!["admin".to_string(), "finance:write".to_string(), "verified".to_string()];
    assert!(check_scopes(&["finance:write"], &granted).is_ok());
}

#[test]
fn test_check_scopes_partial_missing() {
    let granted = vec!["finance:read".to_string()];
    let err = check_scopes(&["finance:read", "finance:write"], &granted).unwrap_err();
    assert_eq!(err.missing, vec!["finance:write"]);
    assert_eq!(err.required, vec!["finance:read", "finance:write"]);
}

#[test]
fn test_check_scopes_all_missing() {
    let err = check_scopes(&["admin", "superuser"], &[]).unwrap_err();
    assert_eq!(err.missing.len(), 2);
    assert!(err.missing.contains(&"admin".to_string()));
    assert!(err.missing.contains(&"superuser".to_string()));
}

#[test]
fn test_scope_denied_display_message() {
    let denied = ScopeDenied {
        required: vec!["finance:write".to_string(), "verified".to_string()],
        missing: vec!["verified".to_string()],
    };
    let msg = denied.to_string();
    assert!(msg.contains("missing required scopes"));
    assert!(msg.contains("verified"));
    assert!(msg.contains("finance:write"));
}

// =============================================================================
// Tool::required_scopes() declarative tests
// =============================================================================

#[test]
fn test_tool_declares_scopes() {
    let tool = TransferFundsTool;
    assert_eq!(tool.required_scopes(), &["finance:write", "verified"]);
}

#[test]
fn test_tool_no_scopes_by_default() {
    let tool = PublicSearchTool;
    assert!(tool.required_scopes().is_empty());
}

#[test]
fn test_admin_tool_declares_admin_scope() {
    let tool = AdminPanelTool;
    assert_eq!(tool.required_scopes(), &["admin"]);
}

// =============================================================================
// ScopeGuard integration tests
// =============================================================================

#[tokio::test]
async fn test_scope_guard_allows_when_scopes_satisfied() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(TransferFundsTool);

    // User has both required scopes
    let user_id = UserId::new("alice").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec!["finance:write", "verified"]);
    let result = protected.execute(ctx, json!({"amount": 100})).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap()["status"], "transferred");
}

#[tokio::test]
async fn test_scope_guard_denies_when_scopes_missing() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(TransferFundsTool);

    // User only has finance:read, missing finance:write and verified
    let user_id = UserId::new("bob").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec!["finance:read"]);
    let result = protected.execute(ctx, json!({"amount": 100})).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("missing required scopes"));
}

#[tokio::test]
async fn test_scope_guard_denies_with_no_scopes() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(AdminPanelTool);

    // User has zero scopes
    let user_id = UserId::new("anonymous").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec![]);
    let result = protected.execute(ctx, json!({})).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_scope_guard_passthrough_for_unscoped_tools() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(PublicSearchTool);

    // Even a user with no scopes can use an unscoped tool
    let user_id = UserId::new("anonymous").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec![]);
    let result = protected.execute(ctx, json!({"query": "hello"})).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap()["query"], "hello");
}

#[tokio::test]
async fn test_scope_guard_superset_scopes_allowed() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(TransferFundsTool);

    // User has more scopes than required — should still work
    let user_id = UserId::new("superuser").unwrap();
    let ctx = ScopedMockContext::create(
        &user_id,
        vec!["admin", "finance:read", "finance:write", "verified"],
    );
    let result = protected.execute(ctx, json!({"amount": 500})).await;

    assert!(result.is_ok());
}

// =============================================================================
// StaticScopeResolver tests
// =============================================================================

#[tokio::test]
async fn test_static_resolver_grants_fixed_scopes() {
    let resolver = StaticScopeResolver::new(vec!["finance:write", "verified"]);
    let guard = ScopeGuard::new(resolver);
    let protected = guard.protect(TransferFundsTool);

    // Context scopes don't matter — static resolver overrides
    let user_id = UserId::new("anyone").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec![]);
    let result = protected.execute(ctx, json!({"amount": 50})).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_static_resolver_insufficient_scopes() {
    let resolver = StaticScopeResolver::new(vec!["finance:read"]);
    let guard = ScopeGuard::new(resolver);
    let protected = guard.protect(TransferFundsTool);

    let user_id = UserId::new("anyone").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec![]);
    let result = protected.execute(ctx, json!({"amount": 50})).await;

    assert!(result.is_err());
}

// =============================================================================
// ScopeToolExt convenience tests
// =============================================================================

#[tokio::test]
async fn test_scope_tool_ext_shorthand() {
    // .with_scope_guard() is the ergonomic one-liner
    let protected = TransferFundsTool.with_scope_guard(ContextScopeResolver);

    let user_id = UserId::new("alice").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec!["finance:write", "verified"]);
    let result = protected.execute(ctx, json!({"amount": 200})).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_scope_tool_ext_denied() {
    let protected = AdminPanelTool.with_scope_guard(ContextScopeResolver);

    let user_id = UserId::new("bob").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec!["user"]);
    let result = protected.execute(ctx, json!({})).await;

    assert!(result.is_err());
}

// =============================================================================
// protect_all() batch tests
// =============================================================================

#[tokio::test]
async fn test_protect_all_mixed_tools() {
    let guard = ScopeGuard::new(ContextScopeResolver);

    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(PublicSearchTool), Arc::new(TransferFundsTool), Arc::new(AdminPanelTool)];

    let protected = guard.protect_all(tools);
    assert_eq!(protected.len(), 3);

    // User with finance scopes but not admin
    let user_id = UserId::new("finance_user").unwrap();
    let ctx = ScopedMockContext::create(&user_id, vec!["finance:write", "verified"]);

    // Public tool: allowed (no scopes required)
    assert!(protected[0].execute(ctx.clone(), json!({})).await.is_ok());

    // Transfer tool: allowed (has finance:write + verified)
    assert!(protected[1].execute(ctx.clone(), json!({"amount": 100})).await.is_ok());

    // Admin tool: denied (missing admin scope)
    assert!(protected[2].execute(ctx, json!({})).await.is_err());
}

// =============================================================================
// Metadata preservation tests
// =============================================================================

#[test]
fn test_scoped_tool_preserves_metadata() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let protected = guard.protect(TransferFundsTool);

    assert_eq!(protected.name(), "transfer_funds");
    assert_eq!(protected.description(), "Transfer funds between accounts");
    assert_eq!(protected.required_scopes(), &["finance:write", "verified"]);
    assert!(!protected.is_long_running());
}

#[test]
fn test_scoped_dyn_tool_preserves_metadata() {
    let guard = ScopeGuard::new(ContextScopeResolver);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(AdminPanelTool)];
    let protected = guard.protect_all(tools);

    assert_eq!(protected[0].name(), "admin_panel");
    assert_eq!(protected[0].description(), "Access admin panel");
    assert_eq!(protected[0].required_scopes(), &["admin"]);
}

// =============================================================================
// Custom ScopeResolver test
// =============================================================================

/// A resolver that maps user IDs to scopes (simulating a database lookup).
struct UserDatabaseResolver {
    user_scopes: std::collections::HashMap<String, Vec<String>>,
}

#[async_trait]
impl ScopeResolver for UserDatabaseResolver {
    async fn resolve(&self, ctx: &dyn ToolContext) -> Vec<String> {
        self.user_scopes.get(ctx.user_id().as_str()).cloned().unwrap_or_default()
    }
}

#[tokio::test]
async fn test_custom_resolver_per_user_scopes() {
    let mut user_scopes = std::collections::HashMap::new();
    user_scopes.insert("alice".to_string(), vec!["admin".to_string()]);
    user_scopes.insert("bob".to_string(), vec!["finance:read".to_string()]);

    let resolver = UserDatabaseResolver { user_scopes };
    let guard = ScopeGuard::new(resolver);
    let protected = guard.protect(AdminPanelTool);

    // Alice has admin scope
    let user_id = UserId::new("alice").unwrap();
    let alice_ctx = ScopedMockContext::create(&user_id, vec![]);
    assert!(protected.execute(alice_ctx, json!({})).await.is_ok());

    // Bob does not have admin scope
    let user_id = UserId::new("bob").unwrap();
    let bob_ctx = ScopedMockContext::create(&user_id, vec![]);
    assert!(protected.execute(bob_ctx, json!({})).await.is_err());
}
