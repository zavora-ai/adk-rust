use adk_core::{
    Agent, CallbackContext, Content, InvocationContext as InvocationContextTrait, Part,
    ReadonlyContext, RunConfig, StreamingMode,
};
use adk_runner::InvocationContext;
use adk_session::{Session, State};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

struct MockState;
impl State for MockState {
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
        "session-789"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-456"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn events(&self) -> &dyn adk_session::Events {
        unimplemented!()
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

// Mock agent for testing
struct MockAgent {
    name: String,
}

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Mock agent"
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

#[test]
fn test_context_creation() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "Hello".to_string() }] };

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent.clone(),
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content.clone(),
        Arc::new(MockSession),
    );

    assert_eq!(ctx.invocation_id(), "inv-123");
    assert_eq!(ctx.agent_name(), "test_agent");
    assert_eq!(ctx.user_id(), "user-456");
    assert_eq!(ctx.app_name(), "test-app");
    assert_eq!(ctx.session_id(), "session-789");
    assert_eq!(ctx.branch(), "");
    assert_eq!(ctx.user_content().role, "user");
}

#[test]
fn test_context_with_branch() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent,
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSession),
    )
    .with_branch("main.sub".to_string());

    assert_eq!(ctx.branch(), "main.sub");
}

#[test]
fn test_context_with_run_config() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let config = RunConfig { streaming_mode: StreamingMode::Enabled };

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent,
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSession),
    )
    .with_run_config(config);

    assert_eq!(ctx.run_config().streaming_mode, StreamingMode::Enabled);
}

#[test]
fn test_context_end_invocation() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent,
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSession),
    );

    assert!(!ctx.ended());
    ctx.end_invocation();
    assert!(ctx.ended());
}

#[test]
fn test_context_agent_access() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent.clone(),
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSession),
    );

    let retrieved_agent = ctx.agent();
    assert_eq!(retrieved_agent.name(), "test_agent");
}

#[test]
fn test_context_optional_services() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent,
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSession),
    );

    assert!(ctx.artifacts().is_none());
    assert!(ctx.memory().is_none());
}
