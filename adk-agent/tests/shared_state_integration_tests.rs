//! Integration tests for ParallelAgent with SharedState.

use adk_agent::ParallelAgent;
use adk_core::{
    Agent, CallbackContext, Content, Event, EventStream, InvocationContext, Memory,
    ReadonlyContext, Result, RunConfig, Session,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Mock infrastructure
// ---------------------------------------------------------------------------

struct MockSession;

impl Session for MockSession {
    fn id(&self) -> &str {
        "test-session"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn state(&self) -> &dyn adk_core::State {
        &DummyState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct DummyState;

impl adk_core::State for DummyState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

struct MockContext {
    user_content: Content,
    session: MockSession,
}

impl MockContext {
    fn new() -> Self {
        Self { user_content: Content::new("user").with_text("test"), session: MockSession }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "test-inv"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Mock agents
// ---------------------------------------------------------------------------

/// Mock agent that publishes a workbook handle to shared state.
struct DataAgent;

#[async_trait]
impl Agent for DataAgent {
    fn name(&self) -> &str {
        "data-agent"
    }
    fn description(&self) -> &str {
        "Creates workbook and publishes handle"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state should be available");

        let s = async_stream::stream! {
            // Simulate workbook creation
            shared
                .set_shared("workbook_id", serde_json::json!("wb-test-123"))
                .await
                .unwrap();
            shared
                .set_shared("data_done", serde_json::json!(true))
                .await
                .unwrap();

            let mut event = Event::new(&invocation_id);
            event.author = "data-agent".to_string();
            event.llm_response.content =
                Some(Content::new("model").with_text("Data written"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

/// Mock agent that waits for workbook handle then proceeds.
struct FormatAgent;

#[async_trait]
impl Agent for FormatAgent {
    fn name(&self) -> &str {
        "format-agent"
    }
    fn description(&self) -> &str {
        "Waits for workbook then formats"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state should be available");

        let s = async_stream::stream! {
            let handle = shared
                .wait_for_key("workbook_id", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(handle, serde_json::json!("wb-test-123"));

            shared
                .set_shared("format_done", serde_json::json!(true))
                .await
                .unwrap();

            let mut event = Event::new(&invocation_id);
            event.author = "format-agent".to_string();
            event.llm_response.content =
                Some(Content::new("model").with_text("Formatting applied"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

/// Mock agent that waits for workbook handle then adds charts.
struct ChartAgent;

#[async_trait]
impl Agent for ChartAgent {
    fn name(&self) -> &str {
        "chart-agent"
    }
    fn description(&self) -> &str {
        "Waits for workbook then adds charts"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state should be available");

        let s = async_stream::stream! {
            let handle = shared
                .wait_for_key("workbook_id", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(handle, serde_json::json!("wb-test-123"));

            shared
                .set_shared("chart_done", serde_json::json!(true))
                .await
                .unwrap();

            let mut event = Event::new(&invocation_id);
            event.author = "chart-agent".to_string();
            event.llm_response.content =
                Some(Content::new("model").with_text("Charts added"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

/// Mock agent that checks shared_state() returns None when not enabled.
struct CheckNoSharedState;

#[async_trait]
impl Agent for CheckNoSharedState {
    fn name(&self) -> &str {
        "check-agent"
    }
    fn description(&self) -> &str {
        "Checks shared state is None"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let has_shared = ctx.shared_state().is_some();

        let s = async_stream::stream! {
            let mut event = Event::new(&invocation_id);
            event.author = "check-agent".to_string();
            event.llm_response.content = Some(Content::new("model").with_text(
                if has_shared { "has_shared" } else { "no_shared" },
            ));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Three sub-agents coordinate via SharedState (Data→Format+Chart).
#[tokio::test]
async fn test_workbook_pattern() {
    let parallel = ParallelAgent::new(
        "workbook-team",
        vec![
            Arc::new(DataAgent) as Arc<dyn Agent>,
            Arc::new(FormatAgent) as Arc<dyn Agent>,
            Arc::new(ChartAgent) as Arc<dyn Agent>,
        ],
    )
    .with_shared_state();

    let ctx = Arc::new(MockContext::new()) as Arc<dyn InvocationContext>;
    let mut stream = parallel.run(ctx).await.unwrap();

    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    // All three agents should have produced events
    assert_eq!(events.len(), 3);

    let authors: Vec<&str> = events.iter().map(|e| e.author.as_str()).collect();
    assert!(authors.contains(&"data-agent"));
    assert!(authors.contains(&"format-agent"));
    assert!(authors.contains(&"chart-agent"));
}

/// Without with_shared_state(), shared_state() returns None.
#[tokio::test]
async fn test_parallel_agent_without_shared_state() {
    let parallel =
        ParallelAgent::new("no-shared", vec![Arc::new(CheckNoSharedState) as Arc<dyn Agent>]);

    let ctx = Arc::new(MockContext::new()) as Arc<dyn InvocationContext>;
    let mut stream = parallel.run(ctx).await.unwrap();

    let event = stream.next().await.unwrap().unwrap();
    let text = event
        .llm_response
        .content
        .as_ref()
        .and_then(|c| c.parts.first())
        .and_then(|p| match p {
            adk_core::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap();
    assert_eq!(text, "no_shared");
}

/// Each run() invocation gets a fresh empty SharedState.
#[tokio::test]
async fn test_fresh_state_per_run() {
    // Agent that writes a key and reads back the snapshot size
    struct WriteAndCountAgent {
        key: String,
    }

    #[async_trait]
    impl Agent for WriteAndCountAgent {
        fn name(&self) -> &str {
            "write-count"
        }
        fn description(&self) -> &str {
            "Writes a key and reports snapshot size"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            let invocation_id = ctx.invocation_id().to_string();
            let shared = ctx.shared_state().expect("shared state");
            let key = self.key.clone();

            let s = async_stream::stream! {
                shared
                    .set_shared(&key, serde_json::json!(1))
                    .await
                    .unwrap();
                let count = shared.snapshot().await.len();

                let mut event = Event::new(&invocation_id);
                event.author = "write-count".to_string();
                event.llm_response.content = Some(
                    Content::new("model").with_text(&format!("{count}")),
                );
                yield Ok(event);
            };
            Ok(Box::pin(s))
        }
    }

    let parallel = ParallelAgent::new(
        "fresh-test",
        vec![Arc::new(WriteAndCountAgent { key: "run-key".to_string() }) as Arc<dyn Agent>],
    )
    .with_shared_state();

    // First run
    let ctx1 = Arc::new(MockContext::new()) as Arc<dyn InvocationContext>;
    let mut stream1 = parallel.run(ctx1).await.unwrap();
    let event1 = stream1.next().await.unwrap().unwrap();

    // Second run — should get a fresh state (count = 1, not 2)
    let ctx2 = Arc::new(MockContext::new()) as Arc<dyn InvocationContext>;
    let mut stream2 = parallel.run(ctx2).await.unwrap();
    let event2 = stream2.next().await.unwrap().unwrap();

    // Both runs should report exactly 1 key in the snapshot
    let get_text = |e: &Event| -> String {
        e.llm_response
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .and_then(|p| match p {
                adk_core::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap()
    };

    assert_eq!(get_text(&event1), "1");
    assert_eq!(get_text(&event2), "1");
}
