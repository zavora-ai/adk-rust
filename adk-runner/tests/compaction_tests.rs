//! Tests for context compaction integration in the runner.

use adk_core::{
    Agent, BaseEventsSummarizer, Content, Event, EventActions, EventCompaction, EventStream,
    EventsCompactionConfig, InvocationContext, Part, Result,
};
use adk_runner::{MutableSession, Runner, RunnerConfig};
use adk_session::{Events, GetRequest, Session, SessionService, State};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ===== Mocks =====

struct MockAgent {
    name: String,
    response_text: String,
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
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let text = self.response_text.clone();
        let name = self.name.clone();
        Ok(Box::pin(futures::stream::once(async move {
            let mut event = Event::new("inv-mock");
            event.author = name;
            event.set_content(Content::new("model").with_text(&text));
            Ok(event)
        })))
    }
}

struct MockEvents {
    events: Vec<Event>,
}

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }
    fn len(&self) -> usize {
        self.events.len()
    }
    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

impl State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

struct MockSession {
    id: String,
    app_name: String,
    user_id: String,
    events: MockEvents,
    state: MockState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> &dyn Events {
        &self.events
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }
}

/// Session service that tracks appended events for verification.
struct TrackingSessionService {
    appended_events: Mutex<Vec<Event>>,
}

impl TrackingSessionService {
    fn new() -> Self {
        Self { appended_events: Mutex::new(Vec::new()) }
    }

    fn appended_events(&self) -> Vec<Event> {
        self.appended_events.lock().unwrap().clone()
    }
}

#[async_trait]
impl SessionService for TrackingSessionService {
    async fn create(&self, _req: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!()
    }
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: req.app_name,
            user_id: req.user_id,
            events: MockEvents { events: vec![] },
            state: MockState,
        }))
    }
    async fn list(&self, _req: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }
    async fn delete(&self, _req: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }
    async fn append_event(&self, _session_id: &str, event: Event) -> Result<()> {
        self.appended_events.lock().unwrap().push(event);
        Ok(())
    }
}

/// A mock summarizer that returns a fixed summary.
struct MockSummarizer {
    summary: String,
    call_count: Mutex<u32>,
}

impl MockSummarizer {
    fn new(summary: &str) -> Self {
        Self { summary: summary.to_string(), call_count: Mutex::new(0) }
    }

    fn call_count(&self) -> u32 {
        *self.call_count.lock().unwrap()
    }
}

#[async_trait]
impl BaseEventsSummarizer for MockSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        *self.call_count.lock().unwrap() += 1;

        if events.is_empty() {
            return Ok(None);
        }

        let summary_content = Content::new("model").with_text(&self.summary);
        let start_timestamp = events.first().unwrap().timestamp;
        let end_timestamp = events.last().unwrap().timestamp;

        let mut event = Event::new("compaction");
        event.author = "system".to_string();
        event.actions = EventActions {
            compaction: Some(EventCompaction {
                start_timestamp,
                end_timestamp,
                compacted_content: summary_content,
            }),
            ..Default::default()
        };

        Ok(Some(event))
    }
}

// ===== Tests =====

/// Test that conversation_history respects compaction events.
/// When a compaction event exists, events before the boundary should be
/// replaced by the compacted summary.
#[test]
fn test_conversation_history_respects_compaction() {
    let base_time = Utc::now() - Duration::minutes(10);

    // Build events: 4 old events + 1 compaction + 2 new events
    let mut old_event_1 = Event::new("inv-1");
    old_event_1.author = "user".to_string();
    old_event_1.timestamp = base_time;
    old_event_1.set_content(Content::new("user").with_text("Old message 1"));

    let mut old_event_2 = Event::new("inv-1");
    old_event_2.author = "assistant".to_string();
    old_event_2.timestamp = base_time + Duration::seconds(1);
    old_event_2.set_content(Content::new("model").with_text("Old response 1"));

    let mut old_event_3 = Event::new("inv-2");
    old_event_3.author = "user".to_string();
    old_event_3.timestamp = base_time + Duration::seconds(2);
    old_event_3.set_content(Content::new("user").with_text("Old message 2"));

    let mut old_event_4 = Event::new("inv-2");
    old_event_4.author = "assistant".to_string();
    old_event_4.timestamp = base_time + Duration::seconds(3);
    old_event_4.set_content(Content::new("model").with_text("Old response 2"));

    // Compaction event summarizing events 1-4
    let mut compaction_event = Event::new("compaction");
    compaction_event.author = "system".to_string();
    compaction_event.timestamp = base_time + Duration::seconds(4);
    compaction_event.actions = EventActions {
        compaction: Some(EventCompaction {
            start_timestamp: old_event_1.timestamp,
            end_timestamp: old_event_4.timestamp,
            compacted_content: Content::new("model").with_text("Summary of old conversation"),
        }),
        ..Default::default()
    };

    // New events after compaction
    let mut new_event_1 = Event::new("inv-3");
    new_event_1.author = "user".to_string();
    new_event_1.timestamp = base_time + Duration::seconds(5);
    new_event_1.set_content(Content::new("user").with_text("New message"));

    let mut new_event_2 = Event::new("inv-3");
    new_event_2.author = "assistant".to_string();
    new_event_2.timestamp = base_time + Duration::seconds(6);
    new_event_2.set_content(Content::new("model").with_text("New response"));

    // Create a mock session with these events
    let mock_session: Arc<dyn adk_session::Session> = Arc::new(MockSession {
        id: "sess-1".to_string(),
        app_name: "test".to_string(),
        user_id: "user-1".to_string(),
        events: MockEvents {
            events: vec![
                old_event_1,
                old_event_2,
                old_event_3,
                old_event_4,
                compaction_event,
                new_event_1,
                new_event_2,
            ],
        },
        state: MockState,
    });

    let mutable = MutableSession::new(mock_session);
    let history = adk_core::Session::conversation_history(&mutable);

    // Should have: summary + new_event_1 + new_event_2 = 3 entries
    assert_eq!(history.len(), 3, "Expected summary + 2 new events, got {}", history.len());

    // First entry should be the compacted summary
    let summary_text = match &history[0].parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(summary_text, "Summary of old conversation");

    // Second entry should be the new user message
    assert_eq!(history[1].role, "user");
    let new_msg = match &history[1].parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(new_msg, "New message");

    // Third entry should be the new model response
    assert_eq!(history[2].role, "model");
}

/// Test that conversation_history works normally when no compaction exists.
#[test]
fn test_conversation_history_without_compaction() {
    let mut event_1 = Event::new("inv-1");
    event_1.author = "user".to_string();
    event_1.set_content(Content::new("user").with_text("Hello"));

    let mut event_2 = Event::new("inv-1");
    event_2.author = "assistant".to_string();
    event_2.set_content(Content::new("model").with_text("Hi there"));

    let mock_session: Arc<dyn adk_session::Session> = Arc::new(MockSession {
        id: "sess-1".to_string(),
        app_name: "test".to_string(),
        user_id: "user-1".to_string(),
        events: MockEvents { events: vec![event_1, event_2] },
        state: MockState,
    });

    let mutable = MutableSession::new(mock_session);
    let history = adk_core::Session::conversation_history(&mutable);

    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[1].role, "model");
}

/// Integration test: runner triggers compaction after reaching the interval.
#[tokio::test]
async fn test_runner_triggers_compaction_at_interval() {
    let summarizer = Arc::new(MockSummarizer::new("Compacted summary"));
    let session_service = Arc::new(TrackingSessionService::new());

    let agent = Arc::new(MockAgent {
        name: "test_agent".to_string(),
        response_text: "Agent response".to_string(),
    });

    let compaction_config = EventsCompactionConfig {
        compaction_interval: 1, // Trigger after every invocation
        overlap_size: 0,
        summarizer: summarizer.clone(),
    };

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: Some(compaction_config),
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let content = Content::new("user").with_text("Hello");
    let mut stream = runner.run("user-1".to_string(), "sess-1".to_string(), content).await.unwrap();

    // Drain the stream
    while let Some(result) = stream.next().await {
        assert!(result.is_ok());
    }

    // The summarizer should have been called
    assert_eq!(summarizer.call_count(), 1, "Summarizer should have been called once");

    // Check that a compaction event was appended to the session
    let appended = session_service.appended_events();
    let compaction_events: Vec<_> =
        appended.iter().filter(|e| e.actions.compaction.is_some()).collect();

    assert_eq!(compaction_events.len(), 1, "Expected exactly one compaction event to be persisted");

    let compaction = compaction_events[0].actions.compaction.as_ref().unwrap();
    let summary_text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(summary_text, "Compacted summary");
}

/// Test that compaction does NOT trigger when interval hasn't been reached.
#[tokio::test]
async fn test_runner_no_compaction_before_interval() {
    let summarizer = Arc::new(MockSummarizer::new("Should not appear"));
    let session_service = Arc::new(TrackingSessionService::new());

    let agent = Arc::new(MockAgent {
        name: "test_agent".to_string(),
        response_text: "Response".to_string(),
    });

    let compaction_config = EventsCompactionConfig {
        compaction_interval: 5, // Only trigger every 5 invocations
        overlap_size: 0,
        summarizer: summarizer.clone(),
    };

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: Some(compaction_config),
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let content = Content::new("user").with_text("Hello");
    let mut stream = runner.run("user-1".to_string(), "sess-1".to_string(), content).await.unwrap();

    while let Some(result) = stream.next().await {
        assert!(result.is_ok());
    }

    // Summarizer should NOT have been called (only 1 invocation, interval is 5)
    assert_eq!(summarizer.call_count(), 0, "Summarizer should not be called before interval");
}
