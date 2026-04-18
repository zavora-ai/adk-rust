//! Preservation Property Test C — Valid Runner Execution Unchanged
//!
//! **Property 6: Preservation — Runner::run() with valid typed IDs**
//!
//! For any valid typed IDs and user content, `Runner::run()` SHALL produce
//! the same event stream, state deltas, and session persistence behavior.
//! `RunnerConfig` SHALL continue to accept `app_name` as `String`.
//!
//! **Validates: Requirements 3.6, 3.7**

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{Agent, Content, EventStream, InvocationContext, Part, Result, SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{Event, Events, GetRequest, Session, SessionService, State};
use async_trait::async_trait;
use futures::StreamExt;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Mock types
// ---------------------------------------------------------------------------

struct MockAgent {
    name: String,
    /// Events this agent will yield when run.
    events_to_yield: Vec<Event>,
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
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let events = self.events_to_yield.clone();
        let invocation_id = ctx.invocation_id().to_string();
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(move |mut e| {
            e.invocation_id = invocation_id.clone();
            Ok(e)
        }))))
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
    fn at(&self, _: usize) -> Option<&Event> {
        None
    }
}

struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _: &str) -> Option<serde_json::Value> {
        None
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

impl State for MockState {
    fn get(&self, _: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _: String, _: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
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

/// Tracks calls to append_event for verification.
struct TrackingSessionService {
    appended_events: Mutex<Vec<Event>>,
}

impl TrackingSessionService {
    fn new() -> Self {
        Self { appended_events: Mutex::new(vec![]) }
    }

    fn appended_count(&self) -> usize {
        self.appended_events.lock().unwrap().len()
    }
}

#[async_trait]
impl SessionService for TrackingSessionService {
    async fn create(&self, _: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!()
    }
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession { id: req.session_id }))
    }
    async fn list(&self, _: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }
    async fn delete(&self, _: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }
    async fn append_event(&self, _session_id: &str, event: Event) -> Result<()> {
        self.appended_events.lock().unwrap().push(event);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_agent_event(author: &str, text: &str) -> Event {
    let mut e = Event::new("placeholder");
    e.author = author.to_string();
    e.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    e
}

fn make_agent_event_with_state_delta(
    author: &str,
    text: &str,
    delta: HashMap<String, serde_json::Value>,
) -> Event {
    let mut e = make_agent_event(author, text);
    e.actions.state_delta = delta;
    e
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: production-hardening, Property 6A: Preservation — Runner streams events**
    ///
    /// *For any* valid user_id and session_id, `Runner::run()` SHALL stream
    /// all events produced by the agent, plus the initial user event persisted
    /// to the session service.
    ///
    /// **Validates: Requirement 3.6**
    #[test]
    fn prop_runner_streams_agent_events(
        user_id_str in "[a-zA-Z0-9]{1,20}",
        session_id_str in "[a-zA-Z0-9]{1,20}",
        agent_event_count in 0usize..5,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let agent_events: Vec<Event> = (0..agent_event_count)
            .map(|i| make_agent_event("test-agent", &format!("response-{i}")))
            .collect();

        let session_service = Arc::new(TrackingSessionService::new());
        let runner = Runner::new(RunnerConfig {
            app_name: "test-app".to_string(),
            agent: Arc::new(MockAgent {
                name: "test-agent".to_string(),
                events_to_yield: agent_events.clone(),
            }),
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: None, intra_compaction_config: None, intra_compaction_summarizer: None,
        }).unwrap();

        let user_id = UserId::new(&user_id_str).unwrap();
        let session_id = SessionId::new(&session_id_str).unwrap();
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "hello".to_string() }],
        };

        let events: Vec<_> = rt.block_on(async {
            let stream = runner.run(user_id, session_id, content).await.unwrap();
            stream.collect::<Vec<_>>().await
        });

        // All streamed events should be Ok
        let ok_events: Vec<Event> = events.into_iter().filter_map(|r: adk_core::Result<Event>| r.ok()).collect();

        // Runner yields the agent's events (user event is persisted but not yielded)
        prop_assert_eq!(ok_events.len(), agent_event_count);

        // Session service should have received: 1 user event + N agent events
        let persisted = session_service.appended_count();
        prop_assert_eq!(persisted, 1 + agent_event_count);
    }
}

/// Verify RunnerConfig still accepts app_name as String (Requirement 3.7).
#[test]
fn runner_config_accepts_string_app_name() {
    let config = RunnerConfig {
        app_name: "my-app".to_string(), // String, not AppName
        agent: Arc::new(MockAgent { name: "a".to_string(), events_to_yield: vec![] }),
        session_service: Arc::new(TrackingSessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    };
    let runner = Runner::new(config);
    assert!(runner.is_ok());
}

/// Verify state deltas from agent events are applied to the mutable session.
#[tokio::test]
async fn runner_applies_state_deltas_from_events() {
    let mut delta = HashMap::new();
    delta.insert("output_key".to_string(), serde_json::json!("result_value"));

    let agent_event = make_agent_event_with_state_delta("test-agent", "done", delta);

    let session_service = Arc::new(TrackingSessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "test-app".to_string(),
        agent: Arc::new(MockAgent {
            name: "test-agent".to_string(),
            events_to_yield: vec![agent_event],
        }),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })
    .unwrap();

    let user_id = UserId::new("user1").unwrap();
    let session_id = SessionId::new("sess1").unwrap();
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "go".to_string() }] };

    let mut stream = runner.run(user_id, session_id, content).await.unwrap();

    // Drain the stream
    while let Some(result) = stream.next().await {
        assert!(result.is_ok());
    }

    // Verify the event with state_delta was persisted
    let persisted = session_service.appended_events.lock().unwrap();
    // 1 user event + 1 agent event
    assert_eq!(persisted.len(), 2);

    // The agent event should have the state_delta
    let agent_evt = &persisted[1];
    assert_eq!(
        agent_evt.actions.state_delta.get("output_key"),
        Some(&serde_json::json!("result_value"))
    );
}
