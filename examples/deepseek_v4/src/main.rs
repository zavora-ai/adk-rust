//! # DeepSeek V4 Example
//!
//! Demonstrates DeepSeek V4 API features with ADK-Rust:
//!
//! 1. **V4 Flash** — fast inference, no thinking
//! 2. **V4 Pro with thinking** — chain-of-thought reasoning at `high` effort
//! 3. **V4 Pro with max effort** — deepest reasoning for hard problems
//! 4. **Thinking + tool calls** — reasoning with function calling
//! 5. **Thinking toggle** — explicitly disable thinking on a V4 model
//! 6. **Multi-turn with thinking** — reasoning across conversation turns
//! 7. **Legacy backward compatibility** — old `deepseek-chat` still works
//!
//! ## V4 Best Practices
//!
//! - Use `v4_flash` for fast, cost-efficient tasks (no thinking by default)
//! - Use `v4_pro` with `ReasoningEffort::High` for standard reasoning
//! - Use `v4_pro` with `ReasoningEffort::Max` for complex agent tasks
//! - In thinking mode, `temperature`/`top_p` are silently ignored by the API
//! - When tool calls occur during thinking, `reasoning_content` is automatically
//!   preserved across turns by ADK's event system
//!
//! ## Run
//!
//! ```bash
//! cd examples/deepseek_v4
//! cp .env.example .env   # add your DEEPSEEK_API_KEY
//! cargo run
//! ```

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Llm};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort, ThinkingMode};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{tool, AdkError};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Tools for the agent
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct CalculateArgs {
    /// Mathematical expression to evaluate (e.g., "2 + 2", "sqrt(144)")
    expression: String,
}

/// Evaluate a mathematical expression and return the result.
#[tool]
async fn calculate(args: CalculateArgs) -> Result<Value, AdkError> {
    // Simple eval for demo — in production use a proper math parser
    let result = match args.expression.trim() {
        "2^10" => "1024",
        "sqrt(144)" => "12",
        "15 * 23" => "345",
        "factorial(7)" => "5040",
        "2^32" => "4294967296",
        expr => {
            return Ok(json!({
                "error": format!("cannot evaluate: {expr}"),
                "hint": "supported: 2^10, sqrt(144), 15*23, factorial(7), 2^32"
            }));
        }
    };
    Ok(json!({ "expression": args.expression, "result": result }))
}

#[derive(Deserialize, JsonSchema)]
struct LookupArgs {
    /// Topic to look up
    topic: String,
}

/// Look up a fact about a topic.
#[tool]
async fn lookup_fact(args: LookupArgs) -> Result<Value, AdkError> {
    let fact = match args.topic.to_lowercase().as_str() {
        t if t.contains("mars") => "Mars has the tallest volcano in the solar system: Olympus Mons at 21.9 km.",
        t if t.contains("ocean") => "The Mariana Trench is the deepest point in the ocean at 10,994 meters.",
        t if t.contains("light") => "Light travels at 299,792,458 meters per second in a vacuum.",
        t if t.contains("dna") => "Human DNA is about 99.9% identical between any two people.",
        _ => "No specific fact found for that topic.",
    };
    Ok(json!({ "topic": args.topic, "fact": fact }))
}

// ---------------------------------------------------------------------------
// Helper: run agent and print response
// ---------------------------------------------------------------------------

async fn run_and_print(
    runner: &Runner,
    session_service: &Arc<InMemorySessionService>,
    session_id: &str,
    input: &str,
) -> anyhow::Result<()> {
    // Create session if it doesn't exist
    let _ = session_service
        .create(CreateRequest {
            app_name: "deepseek-v4-demo".into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await;

    let content = Content::new("user").with_text(input);
    let mut stream = runner.run_str("user", session_id, content).await?;

    let mut saw_thinking = false;
    let mut full_text = String::new();

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                match part {
                    adk_core::Part::Thinking { thinking, .. } => {
                        if !saw_thinking {
                            print!("  💭 Thinking: ");
                            saw_thinking = true;
                        }
                        // Show first 120 chars of reasoning
                        let preview = if thinking.len() > 120 {
                            format!("{}...", &thinking[..120])
                        } else {
                            thinking.clone()
                        };
                        print!("{preview}");
                    }
                    adk_core::Part::Text { text } => {
                        full_text.push_str(text);
                    }
                    adk_core::Part::FunctionCall { name, args, .. } => {
                        println!("  🔧 Tool call: {name}({args})");
                    }
                    _ => {}
                }
            }
        }
    }

    if saw_thinking {
        println!();
    }
    // Truncate long responses
    let display = if full_text.len() > 300 {
        format!("{}...", &full_text[..300])
    } else {
        full_text
    };
    if !display.is_empty() {
        println!("  📝 Response: {display}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: build runner from model + optional tools
// ---------------------------------------------------------------------------

fn build_runner(
    model: Arc<dyn Llm>,
    name: &str,
    instruction: &str,
    tools: bool,
    session_service: Arc<InMemorySessionService>,
) -> anyhow::Result<Runner> {
    let mut builder = LlmAgentBuilder::new(name)
        .description("DeepSeek V4 demo agent")
        .model(model)
        .instruction(instruction);

    if tools {
        builder = builder
            .tool(Arc::new(Calculate))
            .tool(Arc::new(LookupFact));
    }

    let agent = builder.build()?;

    Ok(Runner::builder()
        .app_name("deepseek-v4-demo")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service)
        .build()?)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let api_key =
        std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set — see .env.example");

    println!("╔══════════════════════════════════════════════╗");
    println!("║  DeepSeek V4 Example — ADK-Rust              ║");
    println!("╚══════════════════════════════════════════════╝\n");

    // ── 1. V4 Flash — fast, no thinking ─────────────────────
    println!("── 1. V4 Flash (fast, no thinking) ──────────────");
    let flash = Arc::new(DeepSeekClient::v4_flash(&api_key)?);
    println!("  Model: {}", flash.name());

    let sessions = Arc::new(InMemorySessionService::new());
    let runner = build_runner(
        flash.clone(),
        "flash-agent",
        "You are a concise assistant. Answer in one sentence.",
        false,
        sessions.clone(),
    )?;
    run_and_print(&runner, &sessions, "flash-1", "What is the capital of Kenya?").await?;
    println!();

    // ── 2. V4 Pro with thinking (high effort) ───────────────
    println!("── 2. V4 Pro + thinking (high effort) ───────────");
    let pro_high = Arc::new(DeepSeekClient::new(
        DeepSeekConfig::v4_pro(&api_key).with_reasoning_effort(ReasoningEffort::High),
    )?);
    println!("  Model: {} (reasoning_effort=high)", pro_high.name());

    let sessions2 = Arc::new(InMemorySessionService::new());
    let runner2 = build_runner(
        pro_high,
        "pro-high-agent",
        "You are a reasoning assistant. Show your work briefly.",
        false,
        sessions2.clone(),
    )?;
    run_and_print(
        &runner2,
        &sessions2,
        "pro-high-1",
        "Is 9.11 greater than 9.8? Explain your reasoning.",
    )
    .await?;
    println!();

    // ── 3. V4 Pro with max effort ───────────────────────────
    println!("── 3. V4 Pro + thinking (max effort) ────────────");
    let pro_max = Arc::new(DeepSeekClient::new(
        DeepSeekConfig::v4_pro(&api_key).with_reasoning_effort(ReasoningEffort::Max),
    )?);
    println!("  Model: {} (reasoning_effort=max)", pro_max.name());

    let sessions3 = Arc::new(InMemorySessionService::new());
    let runner3 = build_runner(
        pro_max,
        "pro-max-agent",
        "You are a math expert. Solve problems step by step.",
        false,
        sessions3.clone(),
    )?;
    run_and_print(
        &runner3,
        &sessions3,
        "pro-max-1",
        "How many Rs are in the word 'strawberry'?",
    )
    .await?;
    println!();

    // ── 4. Thinking + tool calls ────────────────────────────
    println!("── 4. V4 Pro + thinking + tool calls ────────────");
    let pro_tools = Arc::new(DeepSeekClient::new(
        DeepSeekConfig::v4_pro(&api_key).with_reasoning_effort(ReasoningEffort::High),
    )?);
    println!("  Model: {} with calculate + lookup_fact tools", pro_tools.name());

    let sessions4 = Arc::new(InMemorySessionService::new());
    let runner4 = build_runner(
        pro_tools,
        "tool-agent",
        "You are a helpful assistant with access to a calculator and fact lookup. Use tools when needed.",
        true,
        sessions4.clone(),
    )?;
    run_and_print(
        &runner4,
        &sessions4,
        "tools-1",
        "What is 2^10 and what is an interesting fact about Mars?",
    )
    .await?;
    println!();

    // ── 5. Thinking explicitly disabled ─────────────────────
    println!("── 5. V4 Pro with thinking disabled ─────────────");
    let pro_no_think = Arc::new(DeepSeekClient::new(
        DeepSeekConfig::v4_pro(&api_key).with_thinking_mode(ThinkingMode::Disabled),
    )?);
    println!(
        "  Model: {} (thinking=disabled)",
        pro_no_think.name()
    );

    let sessions5 = Arc::new(InMemorySessionService::new());
    let runner5 = build_runner(
        pro_no_think,
        "no-think-agent",
        "You are a concise assistant. Answer directly.",
        false,
        sessions5.clone(),
    )?;
    run_and_print(
        &runner5,
        &sessions5,
        "no-think-1",
        "What is the speed of light in km/s?",
    )
    .await?;
    println!();

    // ── 6. Multi-turn with thinking ─────────────────────────
    println!("── 6. Multi-turn conversation with thinking ─────");
    let pro_multi = Arc::new(DeepSeekClient::new(
        DeepSeekConfig::v4_pro(&api_key).with_reasoning_effort(ReasoningEffort::High),
    )?);

    let sessions6 = Arc::new(InMemorySessionService::new());
    let runner6 = build_runner(
        pro_multi,
        "multi-agent",
        "You are a helpful tutor. Build on previous answers.",
        false,
        sessions6.clone(),
    )?;

    println!("  Turn 1:");
    run_and_print(
        &runner6,
        &sessions6,
        "multi-1",
        "What is the Fibonacci sequence?",
    )
    .await?;

    println!("  Turn 2:");
    run_and_print(
        &runner6,
        &sessions6,
        "multi-1", // same session — multi-turn
        "What is the 10th Fibonacci number?",
    )
    .await?;
    println!();

    // ── 7. Legacy backward compatibility ────────────────────
    println!("── 7. Legacy deepseek-chat (backward compat) ────");
    let legacy = Arc::new(DeepSeekClient::chat(&api_key)?);
    println!("  Model: {} (legacy constructor)", legacy.name());

    let sessions7 = Arc::new(InMemorySessionService::new());
    let runner7 = build_runner(
        legacy,
        "legacy-agent",
        "You are a helpful assistant.",
        false,
        sessions7.clone(),
    )?;
    run_and_print(
        &runner7,
        &sessions7,
        "legacy-1",
        "Say hello in three languages.",
    )
    .await?;
    println!();

    println!("✅ DeepSeek V4 example completed successfully.");
    println!("   Features demonstrated:");
    println!("   ✓ V4 Flash (fast, no thinking)");
    println!("   ✓ V4 Pro with thinking (high effort)");
    println!("   ✓ V4 Pro with thinking (max effort)");
    println!("   ✓ Thinking + tool calls (reasoning_content preserved)");
    println!("   ✓ Thinking explicitly disabled");
    println!("   ✓ Multi-turn conversation with thinking");
    println!("   ✓ Legacy deepseek-chat backward compatibility");

    Ok(())
}
