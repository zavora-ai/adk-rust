//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates how events are grouped by invocation_id.
//! It shows how to correlate related events within a single agent invocation.
//!
//! Run modes:
//!   cargo run --example event_grouping -p adk-rust-guide              # Validation mode
//!   cargo run --example event_grouping -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, GetRequest, SessionService};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create a tool to demonstrate multiple events in one invocation
    let calculator = FunctionTool::new(
        "calculator",
        "Performs arithmetic. Parameters: expression (string)",
        |_ctx, args| {
            Box::pin(async move {
                let expr = args["expression"].as_str().unwrap_or("0");
                Ok(json!({"result": format!("Calculated: {}", expr)}))
            })
        },
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("grouping_demo")
            .model(Arc::new(model))
            .instruction("You are a helpful assistant. Use the calculator for math.")
            .tool(Arc::new(calculator))
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    println!("\n=== Event Grouping by Invocation ID ===\n");

    let session_service = Arc::new(InMemorySessionService::new());

    session_service
        .create(CreateRequest {
            app_name: "grouping_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("grouping_session".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "grouping_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;

    // First invocation
    println!("=== First Invocation ===");
    let input1 = Content::new("user").with_text("What is 5 + 3?");
    let mut stream1 = runner
        .run(
            "user_123".to_string(),
            "grouping_session".to_string(),
            input1,
        )
        .await?;

    let mut inv1_id = String::new();
    while let Some(Ok(event)) = stream1.next().await {
        if inv1_id.is_empty() {
            inv1_id = event.invocation_id.clone();
            println!("Invocation ID: {}", inv1_id);
        }
    }
    println!();

    // Second invocation
    println!("=== Second Invocation ===");
    let input2 = Content::new("user").with_text("Now multiply that by 2");
    let mut stream2 = runner
        .run(
            "user_123".to_string(),
            "grouping_session".to_string(),
            input2,
        )
        .await?;

    let mut inv2_id = String::new();
    while let Some(Ok(event)) = stream2.next().await {
        if inv2_id.is_empty() {
            inv2_id = event.invocation_id.clone();
            println!("Invocation ID: {}", inv2_id);
        }
    }
    println!();

    // Analyze event grouping
    let session = session_service
        .get(GetRequest {
            app_name: "grouping_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "grouping_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let events = session.events();

    println!("=== Event Grouping Analysis ===");
    println!("Total events: {}\n", events.len());

    // Group events by invocation_id
    let mut invocation_groups: HashMap<String, Vec<usize>> = HashMap::new();

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            invocation_groups
                .entry(event.invocation_id.clone())
                .or_insert_with(Vec::new)
                .push(i);
        }
    }

    println!("Found {} unique invocations:\n", invocation_groups.len());

    for (inv_id, indices) in &invocation_groups {
        println!("Invocation: {}", inv_id);
        println!("  Events: {}", indices.len());

        for &idx in indices {
            if let Some(event) = events.at(idx) {
                println!(
                    "    [{}] {} at {}",
                    idx + 1,
                    event.author,
                    event.timestamp.format("%H:%M:%S%.3f")
                );
            }
        }
        println!();
    }

    println!("=== Why Invocation Grouping Matters ===");
    println!("• Correlates all events from a single user request");
    println!("• Includes user message, agent response, tool calls, tool results");
    println!("• Enables tracing and debugging of complete interactions");
    println!("• Helps identify performance bottlenecks");
    println!("• Supports audit logging and analytics");
    println!();

    println!("=== Using Invocation IDs ===");
    println!("```rust");
    println!("// Group events by invocation");
    println!("let mut groups: HashMap<String, Vec<Event>> = HashMap::new();");
    println!("for event in events {{");
    println!("    groups.entry(event.invocation_id.clone())");
    println!("        .or_insert_with(Vec::new)");
    println!("        .push(event);");
    println!("}}");
    println!();
    println!("// Find all events for a specific invocation");
    println!("let invocation_events: Vec<_> = events");
    println!("    .filter(|e| e.invocation_id == target_id)");
    println!("    .collect();");
    println!("```");

    print_success("event_grouping");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_grouping -p adk-rust-guide -- chat");

    Ok(())
}
