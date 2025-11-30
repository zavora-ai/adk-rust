//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates control flow signals in events:
//! transfer_to_agent, escalate, and skip_summarization.
//!
//! Run modes:
//!   cargo run --example event_control_flow -p adk-rust-guide              # Validation mode
//!   cargo run --example event_control_flow -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let agent = Arc::new(
        LlmAgentBuilder::new("control_demo")
            .model(Arc::new(model))
            .instruction("You are a helpful assistant.")
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    println!("\n=== Control Flow Signals in Events ===\n");

    println!("EventActions contains three control flow signals:\n");

    println!("1. transfer_to_agent: Option<String>");
    println!("   Purpose: Transfer control to another agent");
    println!("   Usage: Multi-agent workflows, agent handoffs");
    println!("   Example:");
    println!("     actions.transfer_to_agent = Some(\"specialist_agent\".to_string());");
    println!();

    println!("2. escalate: bool");
    println!("   Purpose: Signal escalation to human or supervisor");
    println!("   Usage: Loop termination, error handling");
    println!("   Example:");
    println!("     actions.escalate = true;");
    println!();

    println!("3. skip_summarization: bool");
    println!("   Purpose: Exclude event from conversation summaries");
    println!("   Usage: Internal events, verbose tool outputs");
    println!("   Example:");
    println!("     actions.skip_summarization = true;");
    println!();

    println!("=== Control Flow Example Scenarios ===\n");

    println!("Scenario 1: Agent Transfer");
    println!("  User asks technical question");
    println!("  → General agent recognizes need for specialist");
    println!("  → Event with transfer_to_agent = \"tech_specialist\"");
    println!("  → Runner routes next message to tech_specialist");
    println!();

    println!("Scenario 2: Loop Escalation");
    println!("  LoopAgent attempts task multiple times");
    println!("  → Max iterations reached or unresolvable error");
    println!("  → Event with escalate = true");
    println!("  → Loop terminates, control returns to parent");
    println!();

    println!("Scenario 3: Skip Summarization");
    println!("  Tool returns large debug output");
    println!("  → Event with skip_summarization = true");
    println!("  → Output available in history but not summarized");
    println!("  → Keeps conversation context clean");
    println!();

    println!("=== Detecting Control Signals ===\n");
    println!("```rust");
    println!("if let Some(target) = &event.actions.transfer_to_agent {{");
    println!("    println!(\"Transfer to: {{}}\", target);");
    println!("}}");
    println!();
    println!("if event.actions.escalate {{");
    println!("    println!(\"Escalation requested\");");
    println!("}}");
    println!();
    println!("if event.actions.skip_summarization {{");
    println!("    println!(\"Skip this in summaries\");");
    println!("}}");
    println!("```");

    print_success("event_control_flow");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_control_flow -p adk-rust-guide -- chat");

    Ok(())
}
