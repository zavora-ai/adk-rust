//! # ADK Agent delegating to Kiro CLI via ACP
//!
//! Demonstrates an ADK LLM agent that has Kiro CLI as a tool. The orchestrator
//! (Gemini) decides when to delegate coding tasks to Kiro CLI, which runs as
//! an ACP agent with full tool access.
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

use adk_acp::AcpAgentTool;
use adk_rust::prelude::*;
use adk_rust::Launcher;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY not set"))?;

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Wrap Kiro CLI as an ACP tool the orchestrator can delegate to
    let kiro_tool = AcpAgentTool::new("kiro-cli acp --trust-all-tools")
        .name("kiro")
        .description(
            "Delegate coding tasks to Kiro CLI. Use this for tasks that require \
             reading files, writing code, running commands, or making changes to \
             the project. Send a clear prompt describing what you need done.",
        )
        .working_dir(std::env::current_dir()?);

    let agent = LlmAgentBuilder::new("orchestrator")
        .description("An orchestrator that delegates coding tasks to Kiro CLI")
        .instruction(
            "You are a project manager agent. When the user asks you to do something \
             that involves reading or modifying code, use the 'kiro' tool to delegate \
             the task. For general questions, answer directly without using tools.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(kiro_tool))
        .build()?;

    println!("=== ADK Agent with Kiro CLI as ACP Tool ===");
    println!("The orchestrator (Gemini) delegates coding tasks to Kiro CLI.\n");

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
