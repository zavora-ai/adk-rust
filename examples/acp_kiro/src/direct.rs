//! # Direct ACP connection to Kiro CLI
//!
//! Demonstrates using `adk-acp` to send a prompt directly to Kiro CLI
//! running as an ACP agent. No LLM orchestrator needed — just a direct
//! prompt/response cycle.
//!
//! ## Prerequisites
//!
//! - `kiro-cli` installed and logged in (`kiro-cli login`)
//!
//! ## Run
//!
//! ```bash
//! cargo run --bin acp-kiro-direct
//! ```

use adk_acp::{AcpAgentConfig, prompt_agent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== ADK-ACP: Direct connection to Kiro CLI ===\n");

    let config = AcpAgentConfig::new("kiro-cli acp --trust-all-tools")
        .working_dir(std::env::current_dir()?);

    println!("Spawning Kiro CLI as ACP agent...");
    println!("Prompt: \"What files are in the current directory? List the first 5.\"\n");

    let response = prompt_agent(&config, "What files are in the current directory? List the first 5.").await?;

    println!("--- Response ---");
    println!("{response}");
    println!("--- End ---\n");

    println!("✅ Direct ACP connection to Kiro CLI works.");
    Ok(())
}
