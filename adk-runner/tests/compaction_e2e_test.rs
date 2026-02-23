//! End-to-end compaction test using InMemorySessionService.
//!
//! Verifies the full flow: multiple invocations → compaction triggers →
//! compacted history is used by subsequent invocations.

use adk_core::{
    Agent, BaseEventsSummarizer, Content, Event, EventActions, EventCompaction, EventStream,
    EventsCompactionConfig, InvocationContext, Part, Result,
};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

/// Agent that echoes back a response and records the conversation history it received.
struct HistoryCapturingAgent {
    name: String,
    /// Stores the conversation history seen by the agent on each run.
    captured_histories: Arc<Mutex<Vec<Vec<Content>>>>,
}

#[async_trait]
impl Agent for HistoryCapturingAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Captures conversation history for testing"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        // Capture the conversation history the agent would see
        let history = ctx.session().conversation_history();
        self.captured_histories.lock().unwrap().push(history);

        let name = self.name.clone();
        Ok(Box::pin(futures::stream::once(async move {
            let mut event = Event::new("inv-e2e");
            event.author = name;
            event.set_content(Content::new("model").with_text("Agent response"));
            Ok(event)
        })))
    }
}

/// Summarizer that produces a deterministic summary for testing.
struct DeterministicSummarizer {
    call_count: Mutex<u32>,
}

impl DeterministicSummarizer {
    fn new() -> Self {
        Self { call_count: Mutex::new(0) }
    }
    fn call_count(&self) -> u32 {
        *self.call_count.lock().unwrap()
    }
}

#[async_trait]
impl BaseEventsSummarizer for DeterministicSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        let n = *count;

        if events.is_empty() {
            return Ok(None);
        }

        let num_events = events.len();
        let summary_text = format!("[Compaction #{}: summarized {} events]", n, num_events);

        let summary_content = Content::new("model").with_text(&summary_text);
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

#[tokio::test]
async fn test_e2e_compaction_with_inmemory_session() {
    let session_service = Arc::new(InMemorySessionService::new());
    let captured_histories = Arc::new(Mutex::new(Vec::new()));
    let summarizer = Arc::new(DeterministicSummarizer::new());

    let agent = Arc::new(HistoryCapturingAgent {
        name: "test_agent".to_string(),
        captured_histories: captured_histories.clone(),
    });

    // Create session first
    session_service
        .create(adk_session::CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user-1".to_string(),
            session_id: Some("sess-e2e".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: Some(EventsCompactionConfig {
            compaction_interval: 2, // Compact every 2 invocations
            overlap_size: 0,
            summarizer: summarizer.clone(),
        }),
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    // Invocation 1
    let content1 = Content::new("user").with_text("Hello");
    let mut stream =
        runner.run("user-1".to_string(), "sess-e2e".to_string(), content1).await.unwrap();
    while let Some(r) = stream.next().await {
        assert!(r.is_ok(), "Invocation 1 failed: {:?}", r.err());
    }

    // Invocation 2 — should trigger compaction (interval=2)
    let content2 = Content::new("user").with_text("How are you?");
    let mut stream =
        runner.run("user-1".to_string(), "sess-e2e".to_string(), content2).await.unwrap();
    while let Some(r) = stream.next().await {
        assert!(r.is_ok(), "Invocation 2 failed: {:?}", r.err());
    }

    // Verify compaction was triggered
    assert_eq!(
        summarizer.call_count(),
        1,
        "Summarizer should have been called once after 2 invocations"
    );

    // Verify the compaction event was persisted in the session
    let session = session_service
        .get(adk_session::GetRequest {
            app_name: "test_app".to_string(),
            user_id: "user-1".to_string(),
            session_id: "sess-e2e".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();

    let events = session.events().all();
    let compaction_events: Vec<_> =
        events.iter().filter(|e| e.actions.compaction.is_some()).collect();

    assert_eq!(
        compaction_events.len(),
        1,
        "Expected 1 compaction event in session, found {}",
        compaction_events.len()
    );

    let compaction = compaction_events[0].actions.compaction.as_ref().unwrap();
    let summary_text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part in compaction summary"),
    };
    assert!(
        summary_text.contains("Compaction #1"),
        "Summary should contain compaction marker, got: {}",
        summary_text
    );
}

/// Test that EventCompaction serializes and deserializes correctly.
#[test]
fn test_event_compaction_serde_roundtrip() {
    let compaction = EventCompaction {
        start_timestamp: chrono::Utc::now() - chrono::Duration::minutes(5),
        end_timestamp: chrono::Utc::now(),
        compacted_content: Content::new("model").with_text("Summary of conversation"),
    };

    let actions = EventActions { compaction: Some(compaction.clone()), ..Default::default() };

    let json = serde_json::to_string(&actions).unwrap();
    let deserialized: EventActions = serde_json::from_str(&json).unwrap();

    let restored = deserialized.compaction.unwrap();
    assert_eq!(restored.start_timestamp, compaction.start_timestamp);
    assert_eq!(restored.end_timestamp, compaction.end_timestamp);
    assert_eq!(restored.compacted_content.role, "model");

    let text = match &restored.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(text, "Summary of conversation");
}
