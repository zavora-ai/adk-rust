use adk_core::{
    Agent, BaseEventsSummarizer, Content, Event, EventActions, EventCompaction, EventStream,
    EventsCompactionConfig, InvocationContext, Part, Result, Role,
    types::{AdkIdentity, InvocationId, SessionId, UserId},
};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{GetRequest, InMemorySessionService, Session, SessionService};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

// An agent that records history size to verify compaction
struct HistoryCapturingAgent {
    history_sizes: Arc<Mutex<Vec<usize>>>,
}

#[async_trait]
impl Agent for HistoryCapturingAgent {
    fn name(&self) -> &str {
        "history_capturer"
    }
    fn description(&self) -> &str {
        "Captures history size"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let history = ctx.conversation_history();
        self.history_sizes.lock().unwrap().push(history.len());

        let mut event = Event::new(ctx.invocation_id().clone());
        event.author = "assistant".to_string();
        
        // Trigger compaction manually via actions for testing
        event.actions.compaction = Some(EventCompaction {
            target_invocation_id: ctx.invocation_id().clone(),
            start_timestamp: chrono::Utc::now(),
        });
        
        event.llm_response.content = Some(Content::new(Role::Model).with_text("ack"));

        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

// A summarizer that just returns a fixed string
struct MockSummarizer;

#[async_trait]
impl BaseEventsSummarizer for MockSummarizer {
    async fn summarize_events(&self, _events: &[Event]) -> Result<Option<Event>> {
        let mut event = Event::new(InvocationId::new("summary").unwrap());
        event.author = "summarizer".to_string();
        event.llm_response.content = Some(Content::new(Role::Model).with_text("Summary of conversation"));
        Ok(Some(event))
    }
}

#[tokio::test]
async fn test_compaction_integration() {
    let histories = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(HistoryCapturingAgent { history_sizes: histories.clone() });
    let session_service = Arc::new(InMemorySessionService::new());

    let runner = Runner::new(RunnerConfig {
        app_name: "compaction_test".to_string(),
        agent,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: Some(EventsCompactionConfig {
            compaction_interval: 2,
            overlap_size: 1,
        }),
        cache_capable: None,
        context_cache_config: None,
    })
    .unwrap()
    .with_summarizer(Arc::new(MockSummarizer));

    let user_id = UserId::new("u1").unwrap();
    let session_id = SessionId::new("s1").unwrap();

    // Turn 1: 0 history
    let _ = runner.run(user_id.clone(), session_id.clone(), Content::new(Role::User).with_text("1")).await.unwrap();
    
    // Turn 2: 2 history entries (User 1, Model 1)
    let _ = runner.run(user_id.clone(), session_id.clone(), Content::new(Role::User).with_text("2")).await.unwrap();

    // Verify history sizes recorded by agent
    let sizes = histories.lock().unwrap();
    assert_eq!(sizes[0], 0);
    assert_eq!(sizes[1], 2);

    // Verify session has a compaction event
    let session = session_service.get(GetRequest {
        app_name: "compaction_test".to_string(),
        user_id: user_id.clone(),
        session_id: session_id.clone(),
        num_recent_events: None,
        after: None,
    }).await.unwrap();

    // The summarizer content should be in the session
    assert!(session.events().all().iter().any(|e| e.author == "summarizer"));
}

#[tokio::test]
async fn test_event_serialization_with_compaction() {
    let mut event = Event::new(InvocationId::new("inv-1").unwrap());
    event.actions.compaction = Some(EventCompaction {
        target_invocation_id: InvocationId::new("target").unwrap(),
        start_timestamp: chrono::Utc::now(),
    });

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("compaction"));
    assert!(json.contains("targetInvocationId"));
}
