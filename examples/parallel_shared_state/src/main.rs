//! Parallel Shared State Example — Workbook Pattern
//!
//! Demonstrates three sub-agents coordinating via SharedState:
//! - DataAgent creates a workbook and publishes the handle
//! - FormatAgent waits for the handle, then applies formatting
//! - ChartAgent waits for the handle, then adds charts
//!
//! Run: cargo run --manifest-path examples/parallel_shared_state/Cargo.toml

use adk_agent::ParallelAgent;
use adk_core::{
    Agent, CallbackContext, Content, Event, EventStream, InvocationContext, Memory,
    ReadonlyContext, Result, RunConfig, Session,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

// ── Mock infrastructure ──────────────────────────────────────────────────

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "workbook-app"
    }
    fn user_id(&self) -> &str {
        "user-1"
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

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "workbook-team"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "workbook-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
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
        static RC: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RC.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// ── Sub-agents ───────────────────────────────────────────────────────────

struct DataAgent;

#[async_trait]
impl Agent for DataAgent {
    fn name(&self) -> &str {
        "data-agent"
    }
    fn description(&self) -> &str {
        "Creates workbook and writes cell data"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state required");

        let s = async_stream::stream! {
            println!("  [DataAgent] Creating workbook...");
            tokio::time::sleep(Duration::from_millis(100)).await; // simulate work

            let workbook_id = "wb-excel-2026";
            shared.set_shared("workbook_id", serde_json::json!(workbook_id)).await.unwrap();
            println!("  [DataAgent] Published workbook_id: {workbook_id}");

            println!("  [DataAgent] Writing cell data...");
            tokio::time::sleep(Duration::from_millis(200)).await;
            shared.set_shared("data_complete", serde_json::json!(true)).await.unwrap();
            println!("  [DataAgent] ✓ Data written");

            let mut event = Event::new(&invocation_id);
            event.author = "data-agent".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("Data written to workbook"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

struct FormatAgent;

#[async_trait]
impl Agent for FormatAgent {
    fn name(&self) -> &str {
        "format-agent"
    }
    fn description(&self) -> &str {
        "Applies formatting to workbook"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state required");

        let s = async_stream::stream! {
            println!("  [FormatAgent] Waiting for workbook...");
            let handle = shared.wait_for_key("workbook_id", Duration::from_secs(30)).await.unwrap();
            println!("  [FormatAgent] Got workbook: {handle}");

            println!("  [FormatAgent] Applying formatting...");
            tokio::time::sleep(Duration::from_millis(150)).await;
            shared.set_shared("format_complete", serde_json::json!(true)).await.unwrap();
            println!("  [FormatAgent] ✓ Formatting applied");

            let mut event = Event::new(&invocation_id);
            event.author = "format-agent".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("Formatting applied"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

struct ChartAgent;

#[async_trait]
impl Agent for ChartAgent {
    fn name(&self) -> &str {
        "chart-agent"
    }
    fn description(&self) -> &str {
        "Adds charts to workbook"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let shared = ctx.shared_state().expect("shared state required");

        let s = async_stream::stream! {
            println!("  [ChartAgent] Waiting for workbook...");
            let handle = shared.wait_for_key("workbook_id", Duration::from_secs(30)).await.unwrap();
            println!("  [ChartAgent] Got workbook: {handle}");

            println!("  [ChartAgent] Adding charts...");
            tokio::time::sleep(Duration::from_millis(180)).await;
            shared.set_shared("chart_complete", serde_json::json!(true)).await.unwrap();
            println!("  [ChartAgent] ✓ Charts added");

            let mut event = Event::new(&invocation_id);
            event.author = "chart-agent".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("Charts added"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

// ── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    println!("=== Parallel Shared State: Workbook Pattern ===\n");

    let parallel = ParallelAgent::new(
        "workbook-team",
        vec![
            Arc::new(DataAgent) as Arc<dyn Agent>,
            Arc::new(FormatAgent) as Arc<dyn Agent>,
            Arc::new(ChartAgent) as Arc<dyn Agent>,
        ],
    )
    .with_shared_state()
    .with_description("Coordinates three agents to build a spreadsheet in parallel");

    let ctx = Arc::new(MockContext {
        user_content: Content::new("user").with_text("Create a sales report spreadsheet"),
        session: MockSession,
    }) as Arc<dyn InvocationContext>;

    let mut stream = parallel.run(ctx).await?;

    println!("Running parallel agents...\n");

    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        let event = result?;
        events.push(event);
    }

    println!("\n--- Results ---");
    for event in &events {
        let text = event
            .llm_response
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .and_then(|p| p.text())
            .unwrap_or("(no text)");
        println!("  {}: {text}", event.author);
    }

    println!("\n✓ All {} agents completed successfully", events.len());
    Ok(())
}
