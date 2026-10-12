//! Audit records carry the acting agent's id and the subject it acted for.

use std::sync::{Arc, Mutex};

use adk_auth::{
    AccessControl, AuditEvent, AuditFilter, AuditOutcome, AuditSink, InMemoryAuditSink, Permission,
    ProtectedTool, Role, ScopeGuard, StaticScopeResolver,
};
use adk_core::{
    ActingContext, AgentId, AgentPrincipal, Artifacts, CallbackContext, Content, EventActions,
    MemoryEntry, ReadonlyContext, SubjectRef, Tool, ToolContext,
};
use async_trait::async_trait;
use serde_json::{Value, json};

const AGENT: &str = "spiffe://example.org/agent/treasurer";

struct PayTool;

#[async_trait]
impl Tool for PayTool {
    fn name(&self) -> &str {
        "pay"
    }
    fn description(&self) -> &str {
        "Pays an invoice"
    }
    fn required_scopes(&self) -> &[&str] {
        &["payments:write"]
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "paid": true }))
    }
}

/// A tool context whose acting context is a configured principal, as the runner gives it.
struct IdentifiedContext {
    content: Content,
    actions: Mutex<EventActions>,
}

#[async_trait]
impl ReadonlyContext for IdentifiedContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "treasurer"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "finance"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
    fn acting_context(&self) -> ActingContext {
        ActingContext::new(AgentPrincipal::new(AgentId::new(AGENT).unwrap()).with_tenant("acme"))
            .on_behalf_of(SubjectRef::human("alice"))
    }
}

#[async_trait]
impl CallbackContext for IdentifiedContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for IdentifiedContext {
    fn function_call_id(&self) -> &str {
        "call-1"
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

fn context() -> Arc<dyn ToolContext> {
    Arc::new(IdentifiedContext {
        content: Content::new("user"),
        actions: Mutex::new(EventActions::default()),
    })
}

async fn recorded(sink: &InMemoryAuditSink) -> Vec<AuditEvent> {
    sink.query(&AuditFilter { agent_id: Some(AGENT.into()), ..Default::default() }).await.unwrap()
}

#[tokio::test]
async fn protected_tool_audit_carries_the_agent_id() {
    let sink = Arc::new(InMemoryAuditSink::new());
    let access = AccessControl::builder()
        .role(Role::new("payer").allow(Permission::Tool("pay".into())))
        .assign("alice", "payer")
        .build()
        .unwrap();
    let tool = ProtectedTool::with_audit(PayTool, Arc::new(access), sink.clone());
    tool.execute(context(), json!({})).await.unwrap();

    let events = recorded(&sink).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].agent_id.as_deref(), Some(AGENT));
    assert_eq!(events[0].on_behalf_of, Some(SubjectRef::human("alice")));
    assert_eq!(events[0].tenant_id.as_deref(), Some("acme"));
    assert_eq!(events[0].outcome, AuditOutcome::Allowed);
}

#[tokio::test]
async fn scope_guard_audit_carries_the_agent_id_on_denial() {
    let sink = InMemoryAuditSink::new();
    let shared = Arc::new(sink);
    struct Shared(Arc<InMemoryAuditSink>);
    #[async_trait]
    impl AuditSink for Shared {
        async fn log(&self, event: AuditEvent) -> Result<(), adk_auth::AuthError> {
            self.0.log(event).await
        }
        async fn query(
            &self,
            filter: &AuditFilter,
        ) -> Result<Vec<AuditEvent>, adk_auth::AuthError> {
            self.0.query(filter).await
        }
    }
    let guard = ScopeGuard::with_audit(
        StaticScopeResolver::new(Vec::<String>::new()),
        Shared(shared.clone()),
    );
    let tool = guard.protect(PayTool);
    assert!(tool.execute(context(), json!({})).await.is_err());

    let events = recorded(&shared).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, AuditOutcome::Denied);
    assert_eq!(events[0].agent_id.as_deref(), Some(AGENT));
}

#[test]
fn identity_fields_serialize_and_filter() {
    let event = AuditEvent::tool_access("alice", "pay", AuditOutcome::Allowed).with_acting_context(
        &ActingContext::derived("finance", "treasurer", "alice", adk_core::SubjectKind::Human),
    );
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["agent_id"], "urn:adk:agent:finance/treasurer");
    assert_eq!(json["on_behalf_of"], json!({ "kind": "human", "id": "alice" }));
    let back: AuditEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back.agent_id, event.agent_id);

    let legacy: AuditEvent = serde_json::from_value(json!({
        "timestamp": "2026-01-01T00:00:00Z", "user": "u", "event_type": "tool_access",
        "resource": "r", "outcome": "allowed"
    }))
    .unwrap();
    assert_eq!(legacy.agent_id, None);

    let mine = AuditFilter {
        agent_id: Some("urn:adk:agent:finance/treasurer".into()),
        ..Default::default()
    };
    let theirs =
        AuditFilter { agent_id: Some("urn:adk:agent:finance/other".into()), ..Default::default() };
    assert!(mine.matches(&event));
    assert!(!theirs.matches(&event));
    assert!(!mine.matches(&legacy));
}
