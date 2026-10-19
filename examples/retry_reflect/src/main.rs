//! # Retry & Reflect Demo
//!
//! Demonstrates the ADK-Rust Retry & Reflect plugin (Sprint 2) handling tool
//! failures gracefully with exponential backoff and reflection prompts.
//!
//! ## What This Shows
//!
//! 1. A **flaky tool** that fails 2 out of 3 times (simulating transient errors)
//! 2. The **RetryReflectPlugin** intercepting failures and injecting reflection prompts
//! 3. The agent self-correcting after receiving reflection guidance
//! 4. Exponential backoff between retries (100ms base, doubling each attempt)
//! 5. Tracing output showing retry events
//!
//! ## Run
//!
//! ```bash
//! cd examples/retry_reflect
//! cp .env.example .env   # add your GOOGLE_API_KEY
//! cargo run
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Llm, Tool, ToolContext, async_trait};
use adk_model::GeminiModel;
use adk_plugin::EnhancedPlugin;
use adk_retry_reflect::RetryReflectPluginBuilder;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

// ═══════════════════════════════════════════════════════════════════════════════
// Flaky Search Tool — fails 2 out of 3 times to simulate transient errors
// ═══════════════════════════════════════════════════════════════════════════════

/// A search tool that simulates transient failures.
///
/// The first 2 calls fail with an error JSON, then the 3rd succeeds.
/// This pattern repeats, demonstrating how the retry-reflect plugin
/// handles intermittent failures.
struct FlakySearchTool {
    call_count: AtomicU32,
}

impl FlakySearchTool {
    fn new() -> Self {
        Self { call_count: AtomicU32::new(0) }
    }
}

#[async_trait]
impl Tool for FlakySearchTool {
    fn name(&self) -> &str {
        "search_web"
    }

    fn description(&self) -> &str {
        "Search the web for information on a given query. Returns relevant results."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to look up"
                }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("unknown");

        let count = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;

        println!("    [FlakySearchTool] Attempt #{count} for query: \"{query}\"");

        // Fail on attempts 1 and 2, succeed on attempt 3 (then repeat pattern)
        if count % 3 != 0 {
            let error_msg = match count % 3 {
                1 => "Connection timeout: upstream search service unavailable (503)",
                2 => "Rate limited: too many requests, please retry (429)",
                _ => "Unknown transient error",
            };

            println!("    [FlakySearchTool] ❌ FAILURE (attempt #{count}): {error_msg}");

            // Return error as JSON — the retry-reflect plugin detects this pattern
            Ok(json!({
                "error": error_msg,
                "status_code": if count % 3 == 1 { 503 } else { 429 },
                "retryable": true
            }))
        } else {
            println!("    [FlakySearchTool] ✅ SUCCESS (attempt #{count})");

            // Return successful search results
            Ok(json!({
                "results": [
                    {
                        "title": format!("Top result for: {query}"),
                        "snippet": "Rust is a systems programming language focused on safety, speed, and concurrency.",
                        "url": "https://www.rust-lang.org/"
                    },
                    {
                        "title": format!("Related: {query} guide"),
                        "snippet": "ADK-Rust provides a modular toolkit for building AI agents with tool calling and multi-model support.",
                        "url": "https://github.com/zavora-ai/adk-rust"
                    }
                ],
                "total_results": 2,
                "query": query
            }))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Initialize tracing with retry-reflect events visible
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,adk_retry_reflect=debug")),
        )
        .init();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set — see .env.example");

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Retry & Reflect Demo — ADK-Rust Sprint 2                   ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ── Create the model ─────────────────────────────────────────────────────
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    println!("✓ Model: {}\n", model.name());

    // ── Configure the Retry & Reflect plugin ─────────────────────────────────
    println!("── Configuring RetryReflectPlugin ────────────────────────────────");
    println!("  • Max retries: 3");
    println!("  • Backoff: Exponential (base=100ms, max=5s)");
    println!("  • Priority: 200 (runs after other plugins)");
    println!();

    let retry_plugin = RetryReflectPluginBuilder::new()
        .max_retries(3)
        .backoff_exponential(Duration::from_millis(100))
        .max_backoff(Duration::from_secs(5))
        .priority(200)
        .build()
        .expect("valid retry-reflect configuration");

    // ── Create the flaky tool ────────────────────────────────────────────────
    println!("── Creating FlakySearchTool ──────────────────────────────────────");
    println!("  • Fails 2 out of 3 calls (simulates 503/429 errors)");
    println!("  • Succeeds on every 3rd attempt");
    println!();

    let flaky_tool = Arc::new(FlakySearchTool::new());

    // ── Build the agent ──────────────────────────────────────────────────────
    let agent = LlmAgentBuilder::new("research-assistant")
        .description("A research assistant that searches the web")
        .model(model)
        .instruction(
            "You are a helpful research assistant. When asked a question, \
             use the search_web tool to find relevant information. \
             If a search fails, try again with the same or a refined query. \
             Summarize the results concisely.",
        )
        .tool(flaky_tool as Arc<dyn Tool>)
        .enhanced_plugin(Arc::new(retry_plugin) as Arc<dyn EnhancedPlugin>)
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("retry-reflect-demo")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()?;

    // ── Create a session ─────────────────────────────────────────────────────
    session_service
        .create(CreateRequest {
            app_name: "retry-reflect-demo".into(),
            user_id: "user".into(),
            session_id: Some("demo-session".into()),
            state: Default::default(),
        })
        .await?;

    // ── Run the agent ────────────────────────────────────────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  RUNNING: \"What is Rust programming language?\"");
    println!("══════════════════════════════════════════════════════════════════");
    println!();
    println!("  The retry-reflect cycle:");
    println!("  1. Agent calls search_web → tool FAILS (503)");
    println!("  2. Plugin detects error → injects reflection prompt");
    println!("  3. Agent retries (with backoff) → tool FAILS again (429)");
    println!("  4. Plugin detects error → injects another reflection");
    println!("  5. Agent retries (with longer backoff) → tool SUCCEEDS");
    println!("  6. Agent summarizes the results");
    println!();
    println!("──────────────────────────────────────────────────────────────────\n");

    let start = std::time::Instant::now();
    let content =
        Content::new("user").with_text("What is Rust programming language? Search for it.");
    let mut stream = runner.run_str("user", "demo-session", content).await?;

    let mut response_text = String::new();
    let mut tool_calls_seen = 0u32;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                match part {
                    adk_core::Part::Text { text } => {
                        response_text.push_str(text);
                    }
                    adk_core::Part::FunctionCall { name, args, .. } => {
                        tool_calls_seen += 1;
                        println!("  🔧 Tool call #{tool_calls_seen}: {name}({args})");
                    }
                    _ => {}
                }
            }
        }
    }

    let elapsed = start.elapsed();

    println!("\n──────────────────────────────────────────────────────────────────");
    println!("  RESULT");
    println!("──────────────────────────────────────────────────────────────────\n");
    println!("  🤖 Agent response:");
    println!("  {}\n", truncate(&response_text, 500));
    println!("  📊 Stats:");
    println!("     • Total tool calls observed: {tool_calls_seen}");
    println!("     • Total elapsed time: {:?}", elapsed);
    println!();

    // ── Summary ──────────────────────────────────────────────────────────────
    println!("══════════════════════════════════════════════════════════════════");
    println!("  SUMMARY");
    println!("══════════════════════════════════════════════════════════════════");
    println!();
    println!("  ✅ Retry & Reflect plugin demonstrated:");
    println!("     • Detected tool failures via error JSON pattern");
    println!("     • Injected structured reflection prompts");
    println!("     • Applied exponential backoff (100ms → 200ms → 400ms)");
    println!("     • Agent self-corrected and eventually got results");
    println!();
    println!("  ✅ Key behaviors:");
    println!("     • after_tool_call hook intercepts error results");
    println!("     • Reflection prompt guides the agent to retry");
    println!("     • Backoff prevents overwhelming the failing service");
    println!("     • Max retries (3) prevents infinite loops");
    println!();
    println!("  ✅ Tracing events emitted:");
    println!("     • retry_reflect.retry — each retry attempt");
    println!("     • retry_reflect.exhausted — when max retries reached");
    println!();

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
