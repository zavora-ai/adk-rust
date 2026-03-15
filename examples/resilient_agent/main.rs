//! Resilient Agent Example
//!
//! Demonstrates the resilience features from the browser production hardening
//! spec: retry budgets, circuit breakers, on_tool_error callbacks, and
//! structured ToolOutcome metadata.
//!
//! ## Features Showcased
//!
//! - `RetryBudget` — automatic retry on transient tool failures
//! - `circuit_breaker_threshold()` — disable tools after repeated failures
//! - `on_tool_error()` — fallback results for failed tools
//! - `ToolOutcome` — structured metadata in after-tool callbacks
//!
//! ## Requirements
//!
//! `GOOGLE_API_KEY` environment variable
//!
//! ## Running
//!
//! ```bash
//! cargo run --example resilient_agent
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, CallbackContext, Content, InvocationContext, Part, ReadonlyContext, Result as AdkResult,
    RetryBudget, RunConfig, Session, State, Tool, ToolContext,
};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Flaky tool — fails N times then succeeds (simulates transient errors)
// ---------------------------------------------------------------------------

struct FlakyTool {
    failures_remaining: AtomicU32,
}

impl FlakyTool {
    fn new(fail_count: u32) -> Self {
        Self { failures_remaining: AtomicU32::new(fail_count) }
    }
}

#[async_trait]
impl Tool for FlakyTool {
    fn name(&self) -> &str {
        "flaky_lookup"
    }
    fn description(&self) -> &str {
        "Looks up data but sometimes fails transiently"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" }
            },
            "required": ["query"]
        }))
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let remaining = self.failures_remaining.load(Ordering::SeqCst);
        if remaining > 0 {
            self.failures_remaining.fetch_sub(1, Ordering::SeqCst);
            Err(adk_core::AdkError::Tool(format!(
                "transient network error ({remaining} failures left)"
            )))
        } else {
            let query = args["query"].as_str().unwrap_or("unknown");
            Ok(json!({
                "result": format!("Data for '{query}': temperature 72°F, sunny"),
                "source": "weather_api"
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// Always-failing tool — for circuit breaker demo
// ---------------------------------------------------------------------------

struct BrokenTool;

#[async_trait]
impl Tool for BrokenTool {
    fn name(&self) -> &str {
        "broken_service"
    }
    fn description(&self) -> &str {
        "Fetches the latest data from the external analytics service"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "request": { "type": "string" }
            }
        }))
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        Err(adk_core::AdkError::Tool("service unavailable: connection refused".into()))
    }
}

// ---------------------------------------------------------------------------
// Reliable tool — always works
// ---------------------------------------------------------------------------

struct ReliableTool;

#[async_trait]
impl Tool for ReliableTool {
    fn name(&self) -> &str {
        "reliable_calc"
    }
    fn description(&self) -> &str {
        "A reliable calculator that always works"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "Math expression" }
            },
            "required": ["expression"]
        }))
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let expr = args["expression"].as_str().unwrap_or("0");
        Ok(json!({ "result": format!("Result of '{expr}' = 42") }))
    }
}

// ---------------------------------------------------------------------------
// Minimal session / context boilerplate
// ---------------------------------------------------------------------------

struct SimpleState(std::sync::Mutex<HashMap<String, Value>>);
impl SimpleState {
    fn new() -> Self {
        Self(std::sync::Mutex::new(HashMap::new()))
    }
}
impl State for SimpleState {
    fn get(&self, key: &str) -> Option<Value> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn set(&mut self, key: String, value: Value) {
        self.0.lock().unwrap().insert(key, value);
    }
    fn all(&self) -> HashMap<String, Value> {
        self.0.lock().unwrap().clone()
    }
}

struct Sess(SimpleState);
impl Session for Sess {
    fn id(&self) -> &str {
        "s1"
    }
    fn app_name(&self) -> &str {
        "resilient"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &self.0
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct Ctx {
    agent: Arc<dyn Agent>,
    content: Content,
    config: RunConfig,
    session: Sess,
}

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        self.agent.name()
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "resilient"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}
#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}
#[async_trait]
impl InvocationContext for Ctx {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

async fn run_agent(
    agent: Arc<dyn Agent>,
    task: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: task.to_string() }] };
    let ctx = Arc::new(Ctx {
        agent: agent.clone(),
        content,
        config: RunConfig::default(),
        session: Sess(SimpleState::new()),
    });
    let mut stream = agent.run(ctx).await?;
    let mut response = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(c) = &event.llm_response.content {
                    for part in &c.parts {
                        if let Part::Text { text } = part {
                            response.push_str(text);
                        }
                    }
                }
            }
            Err(e) => return Err(format!("agent error: {e}").into()),
        }
    }
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let _ = dotenvy::dotenv();

    println!("=== Resilient Agent Example ===\n");

    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            println!("GOOGLE_API_KEY not set. export GOOGLE_API_KEY=your_key");
            return Ok(());
        }
    };
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // =========================================================================
    // Example 1: Retry Budget — transient failures are retried automatically
    // =========================================================================
    println!("--- Example 1: Retry Budget ---");
    println!("  FlakyTool will fail 2 times then succeed.");
    println!("  RetryBudget: max_retries=3, delay=100ms\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("retry_agent")
            .model(model.clone())
            .instruction(
                "Use flaky_lookup to answer weather questions. Use reliable_calc for math.",
            )
            .tool(Arc::new(FlakyTool::new(2))) // fails twice, then succeeds
            .tool(Arc::new(ReliableTool))
            // Default retry budget: retry up to 3 times with 100ms delay
            .default_retry_budget(RetryBudget::new(3, Duration::from_millis(100)))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather for Seattle?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 2: Per-tool retry override
    // =========================================================================
    println!("--- Example 2: Per-Tool Retry Override ---");
    println!("  flaky_lookup gets 5 retries; reliable_calc gets 0 (no retry)\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("per_tool_retry_agent")
            .model(model.clone())
            .instruction("Use flaky_lookup for weather. Use reliable_calc for math.")
            .tool(Arc::new(FlakyTool::new(1)))
            .tool(Arc::new(ReliableTool))
            .default_retry_budget(RetryBudget::new(0, Duration::ZERO)) // no retry by default
            .tool_retry_budget("flaky_lookup", RetryBudget::new(5, Duration::from_millis(50)))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather for Portland?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 3: Circuit Breaker — disable tool after repeated failures
    // =========================================================================
    println!("--- Example 3: Circuit Breaker ---");
    println!("  broken_service always fails. Threshold=2.");
    println!("  After 2 consecutive failures, the tool is short-circuited.\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("circuit_breaker_agent")
            .model(model.clone())
            .instruction(
                "You have two tools: broken_service and reliable_calc. \
                 Try broken_service first, then use reliable_calc if it fails. \
                 Answer the user's question.",
            )
            .tool(Arc::new(BrokenTool))
            .tool(Arc::new(ReliableTool))
            .circuit_breaker_threshold(2)
            .build()?,
    );

    let resp = run_agent(agent, "What is 6 times 7?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 4: on_tool_error — fallback result for failed tools
    // =========================================================================
    println!("--- Example 4: on_tool_error Callback ---");
    println!("  When broken_service fails, the callback provides a fallback result.\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("error_hook_agent")
            .model(model.clone())
            .instruction("You MUST use the broken_service tool to look up analytics data. Report what you find.")
            .tool(Arc::new(BrokenTool))
            .on_tool_error(Box::new(|_ctx, tool, _args, error_msg| {
                Box::pin(async move {
                    println!("    [on_tool_error] Tool '{}' failed: {}", tool.name(), error_msg);
                    // Provide a graceful fallback instead of the raw error
                    Ok(Some(json!({
                        "fallback": true,
                        "message": "Service temporarily unavailable. Using cached data.",
                        "cached_result": "Last known value: 42"
                    })))
                })
            }))
            .build()?,
    );

    let resp = run_agent(agent, "Look up the latest data from the service.").await?;
    println!("  Response: {}\n", resp.chars().take(300).collect::<String>());

    // =========================================================================
    // Example 5: ToolOutcome in after-tool callbacks
    // =========================================================================
    println!("--- Example 5: ToolOutcome Metadata ---");
    println!("  After-tool callback inspects structured ToolOutcome.\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("outcome_agent")
            .model(model.clone())
            .instruction("Use reliable_calc to compute 2+2.")
            .tool(Arc::new(ReliableTool))
            .after_tool_callback(Box::new(|ctx: Arc<dyn CallbackContext>| {
                Box::pin(async move {
                    // ToolOutcome is available via CallbackContext::tool_outcome()
                    if let Some(outcome) = ctx.tool_outcome() {
                        println!("    [after_tool] ToolOutcome:");
                        println!("      tool_name: {}", outcome.tool_name);
                        println!("      success: {}", outcome.success);
                        println!("      duration: {:?}", outcome.duration);
                        println!("      attempt: {}", outcome.attempt);
                        if let Some(err) = &outcome.error_message {
                            println!("      error: {err}");
                        }
                    }
                    Ok(None) // None = don't modify the response
                })
            }))
            .build()?,
    );

    let resp = run_agent(agent, "What is 2+2?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    // =========================================================================
    // Example 6: Combined — retry + circuit breaker + error hook
    // =========================================================================
    println!("--- Example 6: Combined Resilience ---");
    println!("  Retry 2x, circuit breaker at 3, with error fallback.\n");

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("combined_agent")
            .model(model.clone())
            .instruction(
                "Use flaky_lookup for weather data. Use reliable_calc for math. \
                 Answer the user's question using available tools.",
            )
            .tool(Arc::new(FlakyTool::new(1)))
            .tool(Arc::new(ReliableTool))
            .default_retry_budget(RetryBudget::new(2, Duration::from_millis(50)))
            .circuit_breaker_threshold(3)
            .on_tool_error(Box::new(|_ctx, tool, _args, err| {
                Box::pin(async move {
                    println!("    [fallback] {}: {err}", tool.name());
                    Ok(Some(json!({ "fallback": true, "message": "using cached data" })))
                })
            }))
            .build()?,
    );

    let resp = run_agent(agent, "What's the weather in Denver?").await?;
    println!("  Response: {}\n", resp.chars().take(200).collect::<String>());

    println!("=== Example Complete ===");
    Ok(())
}
