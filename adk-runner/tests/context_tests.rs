use adk_core::{
    Agent, AppName, CallbackContext, Content, Event, FunctionResponseData,
    InvocationContext as InvocationContextTrait, Part, ReadonlyContext, RunConfig,
    Session as CoreSession, SessionId, StreamingMode, UserId,
};
use adk_runner::{InvocationContext, MutableSession};
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
    state_view: MockSessionStateView,
}

impl MockSessionWithState {
    fn new() -> Self {
        let state_arc = std::sync::Arc::new(std::sync::RwLock::new(HashMap::new()));
        Self { state_view: MockSessionStateView(state_arc) }
    }

    fn with_state(state: HashMap<String, serde_json::Value>) -> Self {
        let state_arc = std::sync::Arc::new(std::sync::RwLock::new(state));
        Self { state_view: MockSessionStateView(state_arc) }
    }
}

impl Session for MockSessionWithState {
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
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "Hello".to_string() }] };

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent.clone(),
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content.clone(),
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

    assert_eq!(ctx.invocation_id(), "inv-123");
    assert_eq!(ctx.agent_name(), "test_agent");
    assert_eq!(ctx.user_id(), "user-456");
    assert_eq!(ctx.app_name(), "test-app");
    assert_eq!(ctx.session_id(), "session-789");
    assert_eq!(ctx.branch(), "");
    assert_eq!(ctx.user_content().role, "user");
}

#[test]
fn test_context_creation_with_typed_ids() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });
    let content = Content::new("user");

    let ctx = InvocationContext::new_typed(
        "inv-typed".to_string(),
        agent,
        UserId::new("user-456").unwrap(),
        AppName::new("test-app").unwrap(),
        SessionId::new("session-789").unwrap(),
        content,
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

    assert_eq!(ctx.invocation_id(), "inv-typed");
    assert_eq!(ctx.user_id(), "user-456");
    assert_eq!(ctx.app_name(), "test-app");
    assert_eq!(ctx.session_id(), "session-789");
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
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap()
    .with_branch("main.sub".to_string());

    assert_eq!(ctx.branch(), "main.sub");
}

#[test]
fn test_context_with_run_config() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let content = Content::new("user");

    let config = RunConfig { streaming_mode: StreamingMode::SSE, ..RunConfig::default() };

    let ctx = InvocationContext::new(
        "inv-123".to_string(),
        agent,
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap()
    .with_run_config(config);

    assert_eq!(ctx.run_config().streaming_mode, StreamingMode::SSE);
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
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

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
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

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
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

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
    let ctx1 = InvocationContext::new(
        "inv-1".to_string(),
        agent.clone(),
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content.clone(),
        Arc::new(MockSessionWithState::new()),
    )
    .unwrap();

    // Get the mutable session from ctx1 and apply a state delta
    let mut delta = HashMap::new();
    delta.insert("from_agent1".to_string(), serde_json::json!("value_from_agent1"));
    ctx1.mutable_session().apply_state_delta(&delta);

    // Create second context sharing the same MutableSession
    let ctx2 = InvocationContext::with_mutable_session(
        "inv-2".to_string(),
        agent.clone(),
        "user-456".to_string(),
        "test-app".to_string(),
        "session-789".to_string(),
        content,
        ctx1.mutable_session().clone(),
    )
    .unwrap();

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
    event1.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "Hello".to_string() }],
    });
    mutable.append_event(event1);

    let mut event2 = Event::new("inv-1");
    event2.author = "assistant".to_string();
    event2.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "Hi there!".to_string() }],
    });
    mutable.append_event(event2);

    assert_eq!(mutable.events_len(), 2);

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
    user_event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "hello".into() }],
    });
    mutable.append_event(user_event);

    // Simulate: assistant with tool call
    let mut assistant_event = Event::new("inv-1");
    assistant_event.author = "my_agent".to_string();
    assistant_event.llm_response.content = Some(Content {
        role: "model".to_string(),
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
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "browser_navigate".into(),
                response: serde_json::json!({"success": true}),
            },
            id: Some("call_1".into()),
        }],
    });
    mutable.append_event(tool_event);

    let history = mutable.conversation_history();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[1].role, "model");
    assert_eq!(history[2].role, "function"); // NOT "model"
}

#[test]
fn conversation_history_maps_agent_events_to_model() {
    // Non-tool agent events should still map to "model"
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    let mut event = Event::new("inv-1");
    event.author = "my_agent".to_string();
    event.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "here are the results".into() }],
    });
    mutable.append_event(event);

    let history = mutable.conversation_history();
    assert_eq!(history[0].role, "model");
}

#[test]
fn conversation_history_for_agent_filters_other_agents_events() {
    // When a sub-agent is invoked after a transfer, it should NOT see
    // the parent agent's tool calls mapped as "model" role.
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // 1. User message
    let mut user_event = Event::new("inv-1");
    user_event.author = "user".to_string();
    user_event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "find me a job".into() }],
    });
    mutable.append_event(user_event);

    // 2. Coordinator calls get_profile tool
    let mut coord_call = Event::new("inv-1");
    coord_call.author = "coordinator".to_string();
    coord_call.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "get_profile".into(),
            args: serde_json::json!({}),
            id: Some("call_1".into()),
            thought_signature: None,
        }],
    });
    mutable.append_event(coord_call);

    // 3. Coordinator's tool response
    let mut coord_tool_resp = Event::new("inv-1");
    coord_tool_resp.author = "coordinator".to_string();
    coord_tool_resp.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "get_profile".into(),
                response: serde_json::json!({"name": "Alice"}),
            },
            id: Some("call_1".into()),
        }],
    });
    mutable.append_event(coord_tool_resp);

    // 4. Coordinator transfers to sourcing_agent
    let mut transfer_event = Event::new("inv-1");
    transfer_event.author = "coordinator".to_string();
    transfer_event.actions.transfer_to_agent = Some("sourcing_agent".to_string());
    transfer_event.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "transferring".into() }],
    });
    mutable.append_event(transfer_event);

    // Unfiltered history should have all 4 events
    let full_history = mutable.conversation_history();
    assert_eq!(full_history.len(), 4);

    // Filtered history for sourcing_agent should only have the user message
    // (coordinator's tool call, tool response, and transfer are excluded)
    use adk_core::Session;
    let filtered = mutable.conversation_history_for_agent("sourcing_agent");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].role, "user");
}

#[test]
fn conversation_history_for_agent_keeps_own_events() {
    // An agent should see its own prior tool calls and responses
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // 1. User message
    let mut user_event = Event::new("inv-1");
    user_event.author = "user".to_string();
    user_event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "search for jobs".into() }],
    });
    mutable.append_event(user_event);

    // 2. sourcing_agent calls search_jobs
    let mut agent_call = Event::new("inv-2");
    agent_call.author = "sourcing_agent".to_string();
    agent_call.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "search_jobs".into(),
            args: serde_json::json!({"query": "rust developer"}),
            id: Some("call_2".into()),
            thought_signature: None,
        }],
    });
    mutable.append_event(agent_call);

    // 3. sourcing_agent's tool response
    let mut tool_resp = Event::new("inv-2");
    tool_resp.author = "sourcing_agent".to_string();
    tool_resp.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "search_jobs".into(),
                response: serde_json::json!({"jobs": []}),
            },
            id: Some("call_2".into()),
        }],
    });
    mutable.append_event(tool_resp);

    // Filtered for sourcing_agent: should see all 3 (user + own call + own response)
    use adk_core::Session;
    let filtered = mutable.conversation_history_for_agent("sourcing_agent");
    assert_eq!(filtered.len(), 3);
    assert_eq!(filtered[0].role, "user");
    assert_eq!(filtered[1].role, "model");
    assert_eq!(filtered[2].role, "function");
}

#[test]
fn conversation_history_for_agent_excludes_other_agents_function_responses() {
    // Function/tool role events from other agents should also be excluded
    // to avoid orphaned responses without their preceding function calls
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // User message
    let mut user_event = Event::new("inv-1");
    user_event.author = "user".to_string();
    user_event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "hello".into() }],
    });
    mutable.append_event(user_event);

    // Other agent's function response (role = "function")
    let mut other_tool = Event::new("inv-1");
    other_tool.author = "other_agent".to_string();
    other_tool.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "some_tool".into(),
                response: serde_json::json!({"data": "value"}),
            },
            id: None,
        }],
    });
    mutable.append_event(other_tool);

    // Other agent's model response (should be filtered out)
    let mut other_model = Event::new("inv-1");
    other_model.author = "other_agent".to_string();
    other_model.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "I did something".into() }],
    });
    mutable.append_event(other_model);

    use adk_core::Session;
    let filtered = mutable.conversation_history_for_agent("my_agent");
    // Only user message kept — other agent's function response and model response both filtered
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].role, "user");
}

#[test]
fn conversation_history_for_agent_double_transfer_sees_own_prior_events() {
    // Scenario: coordinator → sourcing → coordinator → sourcing (second time)
    // The second sourcing invocation should see its own events from the first
    // invocation but NOT the coordinator's events in between.
    let session = Arc::new(MockSessionWithState::new());
    let mutable = MutableSession::new(session);

    // 1. User message
    let mut user_event = Event::new("inv-1");
    user_event.author = "user".to_string();
    user_event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "find me a rust job".into() }],
    });
    mutable.append_event(user_event);

    // 2. Coordinator analyzes request (model role)
    let mut coord_analyze = Event::new("inv-1");
    coord_analyze.author = "coordinator".to_string();
    coord_analyze.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "I'll delegate to sourcing".into() }],
    });
    mutable.append_event(coord_analyze);

    // 3. Coordinator transfers to sourcing_agent
    let mut transfer1 = Event::new("inv-1");
    transfer1.author = "coordinator".to_string();
    transfer1.actions.transfer_to_agent = Some("sourcing_agent".to_string());
    mutable.append_event(transfer1);

    // 4. sourcing_agent calls search_jobs (first invocation)
    let mut sourcing_call1 = Event::new("inv-2");
    sourcing_call1.author = "sourcing_agent".to_string();
    sourcing_call1.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "search_jobs".into(),
            args: serde_json::json!({"query": "rust developer"}),
            id: Some("call_s1".into()),
            thought_signature: None,
        }],
    });
    mutable.append_event(sourcing_call1);

    // 5. sourcing_agent's tool response
    let mut sourcing_resp1 = Event::new("inv-2");
    sourcing_resp1.author = "sourcing_agent".to_string();
    sourcing_resp1.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "search_jobs".into(),
                response: serde_json::json!({"jobs": ["job_1", "job_2"]}),
            },
            id: Some("call_s1".into()),
        }],
    });
    mutable.append_event(sourcing_resp1);

    // 6. sourcing_agent transfers back to coordinator
    let mut transfer_back = Event::new("inv-2");
    transfer_back.author = "sourcing_agent".to_string();
    transfer_back.actions.transfer_to_agent = Some("coordinator".to_string());
    transfer_back.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: "Found 2 jobs, transferring back".into() }],
    });
    mutable.append_event(transfer_back);

    // 7. Coordinator does more work
    let mut coord_work = Event::new("inv-3");
    coord_work.author = "coordinator".to_string();
    coord_work.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "rank_candidates".into(),
            args: serde_json::json!({}),
            id: Some("call_c2".into()),
            thought_signature: None,
        }],
    });
    mutable.append_event(coord_work);

    // 8. Coordinator's tool response
    let mut coord_resp = Event::new("inv-3");
    coord_resp.author = "coordinator".to_string();
    coord_resp.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData {
                name: "rank_candidates".into(),
                response: serde_json::json!({"ranked": ["job_1"]}),
            },
            id: Some("call_c2".into()),
        }],
    });
    mutable.append_event(coord_resp);

    // 9. Coordinator transfers to sourcing_agent AGAIN
    let mut transfer2 = Event::new("inv-3");
    transfer2.author = "coordinator".to_string();
    transfer2.actions.transfer_to_agent = Some("sourcing_agent".to_string());
    mutable.append_event(transfer2);

    // Now: sourcing_agent is invoked a second time.
    // It should see:
    //   - user message (author=user)
    //   - its own search_jobs call from first invocation (author=sourcing_agent)
    //   - its own tool response from first invocation (author=sourcing_agent)
    //   - its own transfer-back text from first invocation (author=sourcing_agent)
    // It should NOT see:
    //   - coordinator's analyze text
    //   - coordinator's transfer events
    //   - coordinator's rank_candidates call/response
    use adk_core::Session;
    let filtered = mutable.conversation_history_for_agent("sourcing_agent");

    // user + sourcing call + sourcing response + sourcing transfer-back text = 4
    assert_eq!(
        filtered.len(),
        4,
        "Expected 4 events (user + 3 own), got {}: {:?}",
        filtered.len(),
        filtered.iter().map(|c| (&c.role, c.parts.first())).collect::<Vec<_>>()
    );
    assert_eq!(filtered[0].role, "user");
    assert_eq!(filtered[1].role, "model"); // own function call
    assert_eq!(filtered[2].role, "function"); // own tool response
    assert_eq!(filtered[3].role, "model"); // own transfer-back text
}
