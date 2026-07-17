//! # Direct ACP connection to Kiro CLI with usage tracking
//!
//! Demonstrates using `adk-acp` to send a prompt directly to Kiro CLI
//! running as an ACP agent, with usage metrics printed after completion.
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
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== ADK-ACP: Direct connection to Kiro CLI ===\n");

    let config = AcpAgentConfig::new("kiro-cli acp")
        .working_dir(std::env::current_dir()?)
        .auto_approve(true);

    let prompt = "What files are in the current directory? List the first 5.";
    println!("Prompt: \"{prompt}\"\n");

    let start = Instant::now();
    let response = prompt_agent(&config, prompt).await?;
    let duration = start.elapsed();

    println!("--- Response ---");
    println!("{response}");
    println!("--- End ---\n");

    println!("📊 Metrics:");
    println!("   Prompt chars:   {}", prompt.len());
    println!("   Response chars: {}", response.len());
    println!("   Latency:        {duration:?}");
    println!("\n✅ Direct ACP connection to Kiro CLI works.");
    Ok(())
}
