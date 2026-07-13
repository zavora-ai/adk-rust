//! # ADK Agent delegating to Kiro CLI via ACP
//!
//! Demonstrates an ADK LLM agent that has Kiro CLI as a tool with:
//! - **Permission policy**: Denies destructive operations (delete, rm, drop)
//! - **Usage tracking**: Records invocation metrics (latency, chars, success rate)
//! - **Telemetry**: All ACP calls emit tracing spans
//!
//! ## Prerequisites
//!
//! - `kiro-cli` installed and logged in (`kiro-cli login`)
//! - `GOOGLE_API_KEY` set (for the orchestrator LLM)
//!
//! ## Run
//!
//! ```bash
//! export GOOGLE_API_KEY=your-key
//! cargo run --bin acp-kiro-delegate
//! ```

use adk_acp::{AcpAgentTool, PermissionDecision, PermissionPolicy, UsageTracker};
use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key =
        std::env::var("GOOGLE_API_KEY").map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY not set"))?;

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Usage tracker — records metrics across all invocations
    let tracker = UsageTracker::new();

    // Permission policy — deny destructive operations, allow everything else
    let policy = PermissionPolicy::Custom(Box::new(|req| {
        let title_lower = req.title.to_lowercase();
        if title_lower.contains("delete")
            || title_lower.contains("rm ")
            || title_lower.contains("drop")
            || title_lower.contains("sudo")
        {
            eprintln!("⚠️  DENIED: {}", req.title);
            PermissionDecision::deny()
        } else {
            PermissionDecision::allow_once()
        }
    }));

    // Wrap Kiro CLI as an ACP tool with permissions and tracking
    let kiro_tool = AcpAgentTool::new("kiro-cli acp")
        .name("kiro")
        .description(
            "Delegate coding tasks to Kiro CLI. Use this for tasks that require \
             reading files, writing code, running commands, or making changes to \
             the project. Send a clear prompt describing what you need done.",
        )
        .working_dir(std::env::current_dir()?)
        .permission_policy(policy)
        .usage_tracker(tracker.clone());

    let agent = LlmAgentBuilder::new("orchestrator")
        .description("An orchestrator that delegates coding tasks to Kiro CLI")
        .instruction(
            "You are a project manager agent. When the user asks you to do something \
             that involves reading or modifying code, use the 'kiro' tool to delegate \
             the task. For general questions, answer directly without using tools.\n\n\
             After delegating a task, summarize what was done in 1-2 sentences.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(kiro_tool))
        .build()?;

    println!("=== ADK Agent with Kiro CLI (ACP) ===");
    println!("Features: permission control, usage tracking, telemetry");
    println!("Destructive operations (delete/rm/drop/sudo) are auto-denied.\n");

    Launcher::new(Arc::new(agent)).run().await?;

    // Print usage stats on exit
    let stats = tracker.stats();
    if stats.total_calls > 0 {
        println!("\n📊 ACP Usage Summary:");
        println!("   Total calls: {}", stats.total_calls);
        println!("   Successful:  {}", stats.successful_calls);
        println!("   Failed:      {}", stats.failed_calls);
        println!("   Total chars sent:     {}", stats.total_prompt_chars);
        println!("   Total chars received: {}", stats.total_response_chars);
        println!("   Total time: {:?}", stats.total_duration);
        if stats.total_calls > 0 {
            println!("   Avg latency: {:?}", stats.total_duration / stats.total_calls as u32);
        }
    }

    Ok(())
}
