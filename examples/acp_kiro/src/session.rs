//! # Persistent ACP session with Kiro CLI
//!
//! Demonstrates session reuse — the agent process stays alive across multiple
//! prompts, preserving context. The second prompt benefits from context
//! established in the first.
//!
//! ## Run
//!
//! ```bash
//! cargo run --bin acp-kiro-session
//! ```

use adk_acp::{AcpAgentConfig, AcpSession, PermissionPolicy};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== ADK-ACP: Persistent Session with Kiro CLI ===\n");

    let config = AcpAgentConfig::new("kiro-cli acp").working_dir(std::env::current_dir()?);

    let policy = Arc::new(PermissionPolicy::AutoApprove);

    println!("Starting persistent session...");
    let mut session = AcpSession::start(config, policy).await?;
    println!("✅ Session started\n");

    // First prompt — establishes context
    println!("─── Prompt 1: Establish context ───");
    let r1 = session
        .prompt("List the Rust source files in adk-acp/src/. Just the filenames, one per line.")
        .await?;
    println!("{}", r1.text);
    println!("  (latency: {:?}, prompt #{})\n", r1.duration, r1.prompt_count);

    // Second prompt — uses context from first
    println!("─── Prompt 2: Use established context ───");
    let r2 =
        session.prompt("Which of those files handles error types? Answer in one sentence.").await?;
    println!("{}", r2.text);
    println!("  (latency: {:?}, prompt #{})\n", r2.duration, r2.prompt_count);

    // Third prompt — deeper context
    println!("─── Prompt 3: Follow-up ───");
    let r3 = session
        .prompt("How many public functions are exported from lib.rs? Just the count.")
        .await?;
    println!("{}", r3.text);
    println!("  (latency: {:?}, prompt #{})\n", r3.duration, r3.prompt_count);

    // Stats
    println!("📊 Session Summary:");
    println!("   Prompts sent: {}", session.prompt_count());
    println!("   Session uptime: {:?}", session.uptime());

    // Clean shutdown
    session.close().await?;
    println!("\n✅ Session closed cleanly.");
    Ok(())
}
