use adk_core::{
    Agent, CallbackContext, Content, Event, 
    InvocationContext, Part, ReadonlyContext, Role, RunConfig,
    StreamingMode, types::{AdkIdentity, InvocationId, SessionId, UserId},
};
use adk_runner::{MutableSession, RunnerContext};
use adk_session::Session;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

// Mock Agent for context testing
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
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> adk_core::Result<adk_core::EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

// Mock Session for context testing
struct MockSessionWithState {
    id: SessionId,
    user_id: UserId,
    state: HashMap<String, serde_json::Value>,
}

impl MockSessionWithState {
    fn new() -> Self {
        Self {
            id: SessionId::new("session-123").unwrap(),
            user_id: UserId::new("user-456").unwrap(),
            state: HashMap::new(),
        }
    }

    fn with_state(state: HashMap<String, serde_json::Value>) -> Self {
        Self {
            id: SessionId::new("session-123").unwrap(),
            user_id: UserId::new("user-456").unwrap(),
            state,
        }
    }
}

impl Session for MockSessionWithState {
    fn id(&self) -> &SessionId {
        &self.id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
    }
    fn state(&self) -> &dyn adk_session::State {
        self
    }
    fn events(&self) -> &dyn adk_session::Events {
        self
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

impl adk_session::State for MockSessionWithState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.state.get(key).cloned()
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.state.clone()
    }
}

impl adk_session::ReadonlyState for MockSessionWithState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.state.get(key).cloned()
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.state.clone()
    }
}

impl adk_session::Events for MockSessionWithState {
    fn all(&self) -> Vec<adk_session::Event> {
        vec![]
    }
    fn len(&self) -> usize {
        0
    }
    fn at(&self, _index: usize) -> Option<&adk_session::Event> {
        None
    }
}

#[test]
fn test_context_creation_and_basic_accessors() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User).with_text("Hello");

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent,
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    );

    assert_eq!(ctx.invocation_id().as_str(), "inv-123");
    assert_eq!(ctx.user_id().as_str(), "user-456");
    assert_eq!(ctx.app_name(), "test-app");
    assert_eq!(ctx.session_id().as_str(), "session-789");
    assert_eq!(ctx.user_content().text(), "Hello");
}

#[test]
fn test_context_with_branch() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent,
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    )
    .with_branch("main.sub".to_string());

    assert_eq!(ctx.branch(), "main.sub");
}

#[test]
fn test_context_with_run_config() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let config = RunConfig { streaming_mode: StreamingMode::SSE, ..RunConfig::default() };

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent,
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    )
    .with_run_config(config);

    assert_eq!(ctx.run_config().streaming_mode, StreamingMode::SSE);
}

#[test]
fn test_context_end_invocation() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent,
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    );

    assert!(!ctx.ended());
    ctx.end_invocation();
    assert!(ctx.ended());
}

#[test]
fn test_context_agent_access() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    );

    let retrieved_agent = ctx.agent();
    assert_eq!(retrieved_agent.name(), "test_agent");
}

#[test]
fn test_context_optional_services() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent,
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    );

    assert!(ctx.artifacts().is_none());
    assert!(ctx.memory().is_none());
}

// ========== MutableSession Tests ==========

#[test]
fn test_mutable_session_state_delta_propagation() {
    let mut initial_state = HashMap::new();
    initial_state.insert("initial_key".to_string(), serde_json::json!("initial_value"));

    let session = Arc::new(MockSessionWithState::with_state(initial_state));
    let mutable = MutableSession::new(session);

    assert_eq!(mutable.state().get("initial_key"), Some(serde_json::json!("initial_value")));

    let mut delta = HashMap::new();
    delta.insert("research_findings".to_string(), serde_json::json!("AI research results"));
    delta.insert("another_key".to_string(), serde_json::json!(42));
    mutable.apply_state_delta(&delta);

    assert_eq!(
        mutable.state().get("research_findings"),
        Some(serde_json::json!("AI research results"))
    );
    assert_eq!(mutable.state().get("another_key"), Some(serde_json::json!(42)));
    assert_eq!(mutable.state().get("initial_key"), Some(serde_json::json!("initial_value")));
}

#[test]
fn test_mutable_session_temp_keys_not_persisted() {
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    let mut delta = HashMap::new();
    delta.insert("temp:scratch".to_string(), serde_json::json!("temporary"));
    delta.insert("permanent".to_string(), serde_json::json!("persisted"));
    mutable.apply_state_delta(&delta);

    assert_eq!(mutable.state().get("temp:scratch"), None);
    assert_eq!(mutable.state().get("permanent"), Some(serde_json::json!("persisted")));
}

#[test]
fn test_mutable_session_shared_across_contexts() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new(Role::User);

    let ctx1 = RunnerContext::new(
        InvocationId::new("inv-1").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content.clone(),
        Arc::new(MockSessionWithState::new()),
    );

    let mut delta = HashMap::new();
    delta.insert("from_agent1".to_string(), serde_json::json!("value_from_agent1"));
    ctx1.mutable_session().apply_state_delta(&delta);

    let ctx2 = RunnerContext::with_mutable_session(
        InvocationId::new("inv-2").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        ctx1.mutable_session().clone(),
    );

    assert_eq!(
        ctx2.session().state().get("from_agent1"),
        Some(serde_json::json!("value_from_agent1"))
    );

    let mut delta2 = HashMap::new();
    delta2.insert("from_agent2".to_string(), serde_json::json!("value_from_agent2"));
    ctx2.mutable_session().apply_state_delta(&delta2);

    assert_eq!(
        ctx1.session().state().get("from_agent2"),
        Some(serde_json::json!("value_from_agent2"))
    );
}

#[test]
fn test_conversation_history_mapping() {
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // Simulate: user message
    let mut user_event = adk_session::Event::new(InvocationId::new("inv-1").unwrap());
    user_event.author = "user".to_string();
    user_event.llm_response.content =
        Some(Content { role: Role::User, parts: vec![Part::text("hello")] });
    mutable.append_event(user_event);

    // Simulate: assistant with tool call
    let mut assistant_event = adk_session::Event::new(InvocationId::new("inv-1").unwrap());
    assistant_event.author = "my_agent".to_string();
    assistant_event.llm_response.content = Some(Content {
        role: Role::Model,
        parts: vec![Part::FunctionCall {
            name: "browser_navigate".to_string(),
            args: serde_json::json!({"url": "https://example.com"}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        }],
    });
    mutable.append_event(assistant_event);

    // Simulate: tool response
    let mut tool_event = adk_session::Event::new(InvocationId::new("inv-1").unwrap());
    tool_event.author = "my_agent".to_string();
    tool_event.llm_response.content = Some(Content {
        role: Role::Tool,
        parts: vec![Part::FunctionResponse {
            name: "browser_navigate".to_string(),
            response: serde_json::json!({"success": true}),
            id: Some("call_1".to_string()),
        }],
    });
    mutable.append_event(tool_event);

    let history = mutable.conversation_history();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Model);
    assert_eq!(history[2].role, Role::Tool);
}
