//! Ambient Cron Agent Example
//!
//! Demonstrates **Ambient Agents** with a CronTrigger and real OpenAI integration.
//! The agent wraps an OpenAI-powered motivational quote generator with lifecycle
//! control (start → pause → resume → stop).
//!
//! The CronTrigger fires every 2 seconds for demonstration purposes. The ambient
//! agent lifecycle is exercised fully:
//!   - Start the ambient agent → status: Running
//!   - Observe trigger events firing (3-4 triggers)
//!   - Pause → status: Paused (triggers are buffered)
//!   - Resume → status: Running (triggers resume)
//!   - Stop → status: Stopped
//!
//! Each trigger drives the agent through a `Runner` via
//! `AmbientAgent::with_invoker`, and whatever the run produces is printed from
//! `AmbientAgent::take_output`.
//!
//! # Usage
//!
//! ```bash
//! cargo run --manifest-path examples/ambient_cron_agent/Cargo.toml
//! ```
//!
//! Set `OPENAI_API_KEY` to see generated quotes. Without a key the lifecycle and
//! trigger wiring still run, and each invocation reports the authentication
//! failure through the output channel rather than failing silently.

use std::sync::Arc;

use adk_agent::ambient::RunnerTriggerConfig;
use adk_agent::{AmbientAgent, AmbientAgentStatus, CronTrigger, LlmAgentBuilder};
use adk_core::Agent;
use adk_model::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use tracing_subscriber::EnvFilter;

// ─── Constants ───────────────────────────────────────────────────────────────

const DEFAULT_MODEL: &str = "gpt-4.1-mini";

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn api_key() -> Option<String> {
    std::env::var("OPENAI_API_KEY").ok().filter(|key| !key.trim().is_empty())
}

fn print_banner() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       Ambient Cron Agent — Event-Driven Background Agent     ║");
    println!("║                                                              ║");
    println!("║  Demonstrates: CronTrigger, AmbientAgent lifecycle           ║");
    println!("║  Pattern: start → observe → pause → resume → stop            ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

fn print_section(title: &str) {
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ {title:<60}│");
    println!("└─────────────────────────────────────────────────────────────┘");
}

fn status_emoji(status: AmbientAgentStatus) -> &'static str {
    match status {
        AmbientAgentStatus::Running => "🟢",
        AmbientAgentStatus::Paused => "🟡",
        AmbientAgentStatus::Stopped => "🔴",
    }
}

// ─── Entry Point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    print_banner();

    // ─── Step 1: Create the underlying LlmAgent ──────────────────────────
    print_section("Step 1: Creating OpenAI-powered quote agent");

    let has_key = api_key().is_some();

    // The AmbientAgent needs an Arc<dyn Agent>. A real key produces real quotes;
    // without one the run still happens and reports its authentication failure.
    let key = api_key().unwrap_or_else(|| "not-set".to_string());
    let model_name = std::env::var("AMBIENT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    if has_key {
        println!("  🔑 OPENAI_API_KEY detected — building real OpenAI agent");
        println!("  📡 Model: {model_name}");
    } else {
        println!("  ℹ️  No OPENAI_API_KEY set — using placeholder key for agent creation");
        println!("  💡 Set OPENAI_API_KEY to enable real LLM invocations");
        println!("  💡 The trigger/lifecycle wiring runs without a key");
    }

    let model = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, &model_name))?);
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("motivational-quote-generator")
            .model(model)
            .instruction(
                "You are a motivational quote generator. Each time you are invoked, \
                 respond with a single unique inspirational quote. Keep it brief — \
                 one to two sentences maximum. Do not repeat quotes.",
            )
            .build()?,
    );

    println!("  ✓ Agent created: \"{}\"", agent.name());

    // ─── Step 2: Create the CronTrigger ──────────────────────────────────
    print_section("Step 2: Creating CronTrigger (every 2 seconds)");

    let trigger = CronTrigger::new("*/2 * * * * *")?;
    println!("  ✓ CronTrigger created: \"*/2 * * * * *\" (fires every 2 seconds)");
    println!("  📋 In production: \"0 9 * * *\" (daily at 9 AM), \"0 */6 * * *\" (every 6h)");

    // ─── Step 3: Create AmbientAgent ─────────────────────────────────────
    print_section("Step 3: Creating AmbientAgent");

    // The Runner owns session handling and executes the agent. `with_invoker`
    // creates a session per trigger, which `Runner::run` does not do on its own.
    let runner: Arc<Runner> = Arc::new(
        Runner::builder()
            .app_name("ambient-cron-agent")
            .agent(Arc::clone(&agent))
            .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
            .build()?,
    );

    let mut ambient = AmbientAgent::new(Arc::clone(&agent), Arc::new(trigger)).with_invoker(
        runner,
        RunnerTriggerConfig::new("system").with_prompt(|event| {
            format!("Give me one motivational quote. Trigger: {}", event.source)
        }),
    );

    // Without this, what each run produced would be logged at debug and dropped.
    let mut outputs = ambient.take_output(32);
    tokio::spawn(async move {
        while let Some(result) = outputs.recv().await {
            match result {
                Ok(event) => {
                    if let Some(content) = event.llm_response.content.as_ref() {
                        let text: String =
                            content.parts.iter().filter_map(|part| part.text()).collect();
                        if !text.trim().is_empty() {
                            println!("  💬 {}", text.trim());
                        }
                    }
                }
                Err(error) => println!("  ⚠️  invocation failed: {error}"),
            }
        }
    });

    let status = ambient.status().await;
    println!("  ✓ AmbientAgent created (initial status: {} {:?})", status_emoji(status), status);

    // ─── Step 4: Start — observe triggers ────────────────────────────────
    print_section("Step 4: Starting ambient agent (observe ~3 triggers)");

    ambient.start().await?;
    let status = ambient.status().await;
    println!("  ✓ Agent started (status: {} {:?})", status_emoji(status), status);

    if has_key {
        println!("  📡 Each trigger invokes the LLM through the Runner");
    } else {
        println!("  📋 Each trigger invokes the agent; failures print below");
    }

    println!("  ⏳ Sleeping 7 seconds to observe triggers...");
    tokio::time::sleep(tokio::time::Duration::from_secs(7)).await;
    println!("  ✓ ~3 triggers should have fired");

    // ─── Step 5: Pause ───────────────────────────────────────────────────
    print_section("Step 5: Pausing ambient agent");

    ambient.pause().await?;
    let status = ambient.status().await;
    println!("  ✓ Agent paused (status: {} {:?})", status_emoji(status), status);
    println!("  📋 Subscription alive but events are buffered, not processed");

    println!("  ⏳ Sleeping 4 seconds while paused (no triggers processed)...");
    tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
    println!("  ✓ No triggers processed while paused");

    // ─── Step 6: Resume ──────────────────────────────────────────────────
    print_section("Step 6: Resuming ambient agent");

    ambient.resume().await?;
    let status = ambient.status().await;
    println!("  ✓ Agent resumed (status: {} {:?})", status_emoji(status), status);

    println!("  ⏳ Sleeping 5 seconds to observe resumed triggers...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    println!("  ✓ ~2 more triggers should have fired");

    // ─── Step 7: Stop ────────────────────────────────────────────────────
    print_section("Step 7: Stopping ambient agent");

    ambient.stop().await?;
    let status = ambient.status().await;
    println!("  ✓ Agent stopped (status: {} {:?})", status_emoji(status), status);
    println!("  📋 Background task cancelled, resources cleaned up");

    // ─── Summary ─────────────────────────────────────────────────────────
    print_section("Lifecycle Summary");

    println!("  ┌──────────────────────────────────────────────────────┐");
    println!("  │  🔴 Stopped  →  start()  →  🟢 Running              │");
    println!("  │  🟢 Running  →  pause()  →  🟡 Paused               │");
    println!("  │  🟡 Paused   →  resume() →  🟢 Running              │");
    println!("  │  🟢 Running  →  stop()   →  🔴 Stopped              │");
    println!("  └──────────────────────────────────────────────────────┘");
    println!();
    println!("  Ambient agents are ideal for:");
    println!("    • Scheduled tasks (cron-based reports, digests, alerts)");
    println!("    • Event-driven processing (webhooks, file watchers)");
    println!("    • Background monitoring and automation");
    println!();
    println!("  Available triggers:");
    println!("    • CronTrigger     — time-based scheduling");
    println!("    • WebhookTrigger  — HTTP POST events");
    println!("    • FileWatchTrigger — filesystem change events");
    println!();

    Ok(())
}
