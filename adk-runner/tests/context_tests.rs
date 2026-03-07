use adk_core::types::{InvocationId, SessionId, UserId};
use adk_core::{
    Agent, CallbackContext, Content, Event, InvocationContext as InvocationContextTrait, Part,
    ReadonlyContext, RunConfig, Session as CoreSession, StreamingMode,
};
use adk_runner::{MutableSession, RunnerContext};
use adk_session::{Events, Session, State};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

struct MockEvents;
impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        Vec::new()
    }
    fn len(&self) -> usize {
        0
    }
    fn at(&self, _index: usize) -> Option<&Event> {
        None
    }
}

// State implementation that wraps the Arc<RwLock<HashMap>>
struct MockSessionStateView(std::sync::Arc<std::sync::RwLock<HashMap<String, serde_json::Value>>>);

impl State for MockSessionStateView {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.read().unwrap().get(key).cloned()
    }
    fn set(&mut self, key: String, value: serde_json::Value) {
        self.0.write().unwrap().insert(key, value);
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.read().unwrap().clone()
    }
}

// MockSession that supports returning a reference to state
struct MockSessionWithState {
    session_id: SessionId,
    user_id: UserId,
    state_view: MockSessionStateView,
}

impl MockSessionWithState {
    fn new() -> Self {
        let state_arc = std::sync::Arc::new(std::sync::RwLock::new(HashMap::new()));
        Self {
            session_id: SessionId::new("session-789").unwrap(),
            user_id: UserId::new("user-456").unwrap(),
            state_view: MockSessionStateView(state_arc),
        }
    }

    fn with_state(state: HashMap<String, serde_json::Value>) -> Self {
        let state_arc = std::sync::Arc::new(std::sync::RwLock::new(state));
        Self {
            session_id: SessionId::new("session-789").unwrap(),
            user_id: UserId::new("user-456").unwrap(),
            state_view: MockSessionStateView(state_arc),
        }
    }
}

impl Session for MockSessionWithState {
    fn id(&self) -> &SessionId {
        &self.session_id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state_view
    }
    fn events(&self) -> &dyn adk_session::Events {
        static MOCK_EVENTS: MockEvents = MockEvents;
        &MOCK_EVENTS
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
        Content { role: adk_core::Role::User, parts: vec![Part::Text("Hello".to_string())] };

    let ctx = RunnerContext::new(
        InvocationId::new("inv-123").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content.clone(),
        Arc::new(MockSessionWithState::new()),
    );

    assert_eq!(ctx.invocation_id().as_str(), "inv-123");
    assert_eq!(ctx.agent_name(), "test_agent");
    assert_eq!(ctx.user_id().as_str(), "user-456");
    assert_eq!(ctx.app_name(), "test-app");
    assert_eq!(ctx.session_id().as_str(), "session-789");
    assert_eq!(ctx.branch(), "main");
    assert_eq!(ctx.user_content().role, adk_core::Role::User);
}

#[test]
fn test_context_with_branch() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

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

    let content = Content::new("user");

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

    let content = Content::new("user");

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

    let content = Content::new("user");

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

    let content = Content::new("user");

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
    // This is the key test for the bug fix: state_delta should be visible to readers
    let mut initial_state = HashMap::new();
    initial_state.insert("initial_key".to_string(), serde_json::json!("initial_value"));

    let session = Arc::new(MockSessionWithState::with_state(initial_state));
    let mutable = MutableSession::new(session);

    // Verify initial state is accessible
    assert_eq!(mutable.state().get("initial_key"), Some(serde_json::json!("initial_value")));

    // Apply state delta (simulating what happens when an agent with output_key yields an event)
    let mut delta = HashMap::new();
    delta.insert("research_findings".to_string(), serde_json::json!("AI research results"));
    delta.insert("another_key".to_string(), serde_json::json!(42));
    mutable.apply_state_delta(&delta);

    // Verify new state is visible immediately (this is the bug fix verification)
    assert_eq!(
        mutable.state().get("research_findings"),
        Some(serde_json::json!("AI research results"))
    );
    assert_eq!(mutable.state().get("another_key"), Some(serde_json::json!(42)));

    // Original state should still be there
    assert_eq!(mutable.state().get("initial_key"), Some(serde_json::json!("initial_value")));
}

#[test]
fn test_mutable_session_temp_keys_not_persisted() {
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // Apply delta with temp: prefixed keys
    let mut delta = HashMap::new();
    delta.insert("temp:scratch".to_string(), serde_json::json!("temporary"));
    delta.insert("permanent".to_string(), serde_json::json!("persisted"));
    mutable.apply_state_delta(&delta);

    // temp: keys should NOT be stored
    assert_eq!(mutable.state().get("temp:scratch"), None);
    // Regular keys should be stored
    assert_eq!(mutable.state().get("permanent"), Some(serde_json::json!("persisted")));
}

#[test]
fn test_mutable_session_shared_across_contexts() {
    // Test that two InvocationContexts sharing the same MutableSession see each other's changes
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new("user");

    // Create first context
    let ctx1 = RunnerContext::new(
        InvocationId::new("inv-1").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content.clone(),
        Arc::new(MockSessionWithState::new()),
    );

    // Get the mutable session from ctx1 and apply a state delta
    let mut delta = HashMap::new();
    delta.insert("from_agent1".to_string(), serde_json::json!("value_from_agent1"));
    ctx1.mutable_session().apply_state_delta(&delta);

    // Create second context sharing the same MutableSession
    let ctx2 = RunnerContext::with_mutable_session(
        InvocationId::new("inv-2").unwrap(),
        agent.clone(),
        UserId::new("user-456").unwrap(),
        "test-app".to_string(),
        SessionId::new("session-789").unwrap(),
        content,
        ctx1.mutable_session().clone(),
    );

    // ctx2 should see the state set by ctx1
    assert_eq!(
        ctx2.session().state().get("from_agent1"),
        Some(serde_json::json!("value_from_agent1"))
    );

    // Apply another delta via ctx2
    let mut delta2 = HashMap::new();
    delta2.insert("from_agent2".to_string(), serde_json::json!("value_from_agent2"));
    ctx2.mutable_session().apply_state_delta(&delta2);

    // ctx1 should also see the state set by ctx2
    assert_eq!(
        ctx1.session().state().get("from_agent2"),
        Some(serde_json::json!("value_from_agent2"))
    );
}

#[test]
fn test_mutable_session_event_accumulation() {
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // Initially no events
    assert_eq!(mutable.conversation_history().len(), 0);

    // Append some events
    let mut event1 = Event::new("inv-1");
    event1.author = "user".to_string();
    event1.llm_response.content =
        Some(Content { role: adk_core::Role::User, parts: vec![Part::Text("Hello".to_string())] });
    mutable.append_event(event1);

    let mut event2 = Event::new("inv-1");
    event2.author = "assistant".to_string();
    event2.llm_response.content = Some(Content {
        role: adk_core::Role::Model,
        parts: vec![Part::Text("Hi there!".to_string())],
    });
    mutable.append_event(event2);

    // Check conversation history
    let history = mutable.conversation_history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[1].role, "model");
}

#[test]
fn test_mutable_session_state_all() {
    let mut initial_state = HashMap::new();
    initial_state.insert("key1".to_string(), serde_json::json!("value1"));

    let session = Arc::new(MockSessionWithState::with_state(initial_state));
    let mutable = MutableSession::new(session);

    let mut delta = HashMap::new();
    delta.insert("key2".to_string(), serde_json::json!("value2"));
    mutable.apply_state_delta(&delta);

    let all_state = mutable.state().all();
    assert_eq!(all_state.len(), 2);
    assert_eq!(all_state.get("key1"), Some(&serde_json::json!("value1")));
    assert_eq!(all_state.get("key2"), Some(&serde_json::json!("value2")));
}

#[test]
fn conversation_history_preserves_tool_role() {
    // Tool response events with role "function" should NOT be overwritten to "model"
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // Simulate: user message
    let mut user_event = Event::new("inv-1");
    user_event.author = "user".to_string();
    user_event.llm_response.content =
        Some(Content { role: adk_core::Role::User, parts: vec![Part::Text("hello".into())] });
    mutable.append_event(user_event);

    // Simulate: assistant with tool call
    let mut assistant_event = Event::new("inv-1");
    assistant_event.author = "my_agent".to_string();
    assistant_event.llm_response.content = Some(Content {
        role: adk_core::Role::Model,
        parts: vec![Part::FunctionCall {
            name: "browser_navigate".into(),
            args: serde_json::json!({"url": "https://example.com"}),
            id: Some("call_1".into()),
            thought_signature: None,
        }],
    });
    mutable.append_event(assistant_event);

    // Simulate: tool response
    let mut tool_event = Event::new("inv-1");
    tool_event.author = "my_agent".to_string();
    tool_event.llm_response.content = Some(Content {
        role: adk_core::Role::Custom("function".to_string()),
        parts: vec![Part::FunctionResponse {
            name: "browser_navigate".into(),
            response: serde_json::json!({"success": true}),
            id: Some("call_1".into()),
        }],
    });
    mutable.append_event(tool_event);

    let history = mutable.conversation_history();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[1].role, "model");
    assert_eq!(history[2].role, adk_core::Role::Custom("function".to_string())); // NOT "model"
}

#[test]
fn conversation_history_maps_agent_events_to_model() {
    // Non-tool agent events should still map to "model"
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    let mut event = Event::new("inv-1");
    event.author = "my_agent".to_string();
    event.llm_response.content = Some(Content {
        role: adk_core::Role::Model,
        parts: vec![Part::Text("here are the results".into())],
    });
    mutable.append_event(event);

    let history = mutable.conversation_history();
    assert_eq!(history[0].role, "model");
}
