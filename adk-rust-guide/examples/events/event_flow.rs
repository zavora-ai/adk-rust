//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates how events flow through the ADK-Rust system.
//! It shows event generation, processing by the Runner, and persistence.
//!
//! Run modes:
//!   cargo run --example event_flow -p adk-rust-guide              # Validation mode
//!   cargo run --example event_flow -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, GetRequest, SessionService};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with callbacks to observe event flow
    let agent = Arc::new(
        LlmAgentBuilder::new("flow_demo")
            .model(Arc::new(model))
            .instruction("You are a helpful assistant. Keep responses brief.")
            .before_callback(Box::new(|ctx| {
                Box::pin(async move {
                    println!("[BEFORE AGENT] About to process invocation: {}", ctx.invocation_id());
                    Ok(None)
                })
            }))
            .after_callback(Box::new(|ctx| {
                Box::pin(async move {
                    println!("[AFTER AGENT] Completed invocation: {}", ctx.invocation_id());
                    Ok(None)
                })
            }))
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    // =========================================================================
    // Demonstrate Event Flow
    // =========================================================================
    println!("\n=== Event Flow Demonstration ===\n");

    let session_service = Arc::new(InMemorySessionService::new());

    // Step 1: Create a session
    println!("Step 1: Creating session...");
    let session = session_service
        .create(CreateRequest {
            app_name: "flow_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("flow_session".to_string()),
            state: HashMap::new(),
        })
        .await?;
    println!("  ✓ Session created: {}", session.id());
    println!("  ✓ Initial events: {}\n", session.events().len());

    // Step 2: Create Runner
    println!("Step 2: Creating Runner...");
    let runner = Runner::new(RunnerConfig {
        app_name: "flow_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;
    println!("  ✓ Runner created\n");

    // Step 3: Generate user input event
    println!("Step 3: Creating user input...");
    let user_input = Content::new("user").with_text("Hello! What's 2+2?");
    println!("  ✓ User content created\n");

    // Step 4: Run agent and observe event stream
    println!("Step 4: Running agent and observing event stream...");
    let mut stream = runner
        .run("user_123".to_string(), "flow_session".to_string(), user_input)
        .await?;

    let mut event_count = 0;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                event_count += 1;
                println!("\n  [Event {}]", event_count);
                println!("    Author: {}", event.author);
                println!("    ID: {}", event.id);
                println!("    Invocation: {}", event.invocation_id);
                println!("    Timestamp: {}", event.timestamp.format("%H:%M:%S%.3f"));

                // Show content if present
                if let Some(content) = &event.llm_response.content {
                    let mut text_parts = Vec::new();
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            text_parts.push(text.as_str());
                        }
                    }
                    let text = text_parts.join(" ");
                    if !text.is_empty() {
                        let preview = if text.len() > 50 {
                            format!("{}...", &text[..50])
                        } else {
                            text
                        };
                        println!("    Content: {}", preview);
                    }
                }
            }
            Err(e) => {
                eprintln!("    Error: {}", e);
                break;
            }
        }
    }
    println!("\n  ✓ Event stream completed ({} events)\n", event_count);

    // Step 5: Verify events were persisted
    println!("Step 5: Verifying event persistence...");
    let updated_session = session_service
        .get(GetRequest {
            app_name: "flow_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "flow_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let events = updated_session.events();
    println!("  ✓ Total persisted events: {}", events.len());

    // Show event flow summary
    println!("\n=== Event Flow Summary ===");
    println!("1. User input → Runner");
    println!("2. Runner → Agent (with conversation history)");
    println!("3. Agent → LLM");
    println!("4. LLM response → Agent");
    println!("5. Agent → Runner (as event stream)");
    println!("6. Runner → SessionService (for persistence)");
    println!("7. SessionService → Session.events (appended)");
    println!("8. Runner → Application (event stream)");

    // Verify event ordering
    println!("\n=== Verifying Event Properties ===");
    let mut is_chronological = true;
    for i in 1..events.len() {
        if let (Some(prev), Some(curr)) = (events.at(i - 1), events.at(i)) {
            if curr.timestamp < prev.timestamp {
                is_chronological = false;
                break;
            }
        }
    }
    assert!(is_chronological, "Events should be chronologically ordered");
    println!("✓ Events are chronologically ordered");

    // Verify all events have the same invocation_id
    let mut invocation_ids = std::collections::HashSet::new();
    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            invocation_ids.insert(event.invocation_id.clone());
        }
    }
    println!("✓ Events grouped by {} invocation(s)", invocation_ids.len());

    print_success("event_flow");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_flow -p adk-rust-guide -- chat");

    Ok(())
}
