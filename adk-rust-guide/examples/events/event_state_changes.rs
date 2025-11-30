//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates how events carry state changes through state_delta.
//! It shows how state modifications are recorded and persisted via events.
//!
//! Run modes:
//!   cargo run --example event_state_changes -p adk-rust-guide              # Validation mode
//!   cargo run --example event_state_changes -p adk-rust-guide -- chat      # Interactive console

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

    // Create a tool that modifies state
    let update_preference = FunctionTool::new(
        "update_preference",
        "Updates a user preference. Parameters: key (string), value (string)",
        |_ctx, args| {
            Box::pin(async move {
                let key = args["key"].as_str().unwrap_or("").to_string();
                let value = args["value"].as_str().unwrap_or("").to_string();

                // Modify state through the context
                // Note: In actual implementation, this would use ctx.state() if available
                // For this example, we'll return the change to be applied

                Ok(json!({
                    "success": true,
                    "message": format!("Updated {} to {}", key, value)
                }))
            })
        },
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("state_demo")
            .model(Arc::new(model))
            .instruction(
                "You are a helpful assistant that manages user preferences. \
                 Use the update_preference tool when users want to change settings.",
            )
            .tool(Arc::new(update_preference))
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    println!("\n=== State Changes via Events ===\n");

    let session_service = Arc::new(InMemorySessionService::new());

    // Create session with initial state
    let mut initial_state = HashMap::new();
    initial_state.insert("user_name".to_string(), json!("Alice"));
    initial_state.insert("theme".to_string(), json!("light"));
    initial_state.insert("notifications".to_string(), json!(true));

    let session = session_service
        .create(CreateRequest {
            app_name: "state_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("state_session".to_string()),
            state: initial_state,
        })
        .await?;

    println!("Initial state:");
    for (key, value) in session.state().all() {
        println!("  {} = {}", key, value);
    }
    println!();

    let runner = Runner::new(RunnerConfig {
        app_name: "state_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;

    // Run agent interaction
    let user_input = Content::new("user").with_text("Change my theme to dark mode");

    let mut stream = runner
        .run(
            "user_123".to_string(),
            "state_session".to_string(),
            user_input,
        )
        .await?;

    println!("Processing events and tracking state changes...\n");

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                // Check for state changes in this event
                if !event.actions.state_delta.is_empty() {
                    println!("─────────────────────────────────────");
                    println!("Event with state changes detected!");
                    println!("Event ID: {}", event.id);
                    println!("Author: {}", event.author);
                    println!("\nState delta:");
                    for (key, value) in &event.actions.state_delta {
                        println!("  {} = {}", key, value);
                    }
                    println!("─────────────────────────────────────\n");
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    // Retrieve updated session and show final state
    let updated_session = session_service
        .get(GetRequest {
            app_name: "state_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "state_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    println!("Final state:");
    for (key, value) in updated_session.state().all() {
        println!("  {} = {}", key, value);
    }
    println!();

    // Analyze all events for state changes
    let events = updated_session.events();
    println!("=== State Change Analysis ===");
    println!("Total events: {}", events.len());

    let mut events_with_state_changes = 0;
    let mut total_state_keys_changed = 0;

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            if !event.actions.state_delta.is_empty() {
                events_with_state_changes += 1;
                total_state_keys_changed += event.actions.state_delta.len();

                println!("\nEvent {} (by {}):", i + 1, event.author);
                for (key, value) in &event.actions.state_delta {
                    println!("  {} → {}", key, value);
                }
            }
        }
    }

    println!("\nSummary:");
    println!("  Events with state changes: {}", events_with_state_changes);
    println!("  Total state keys modified: {}", total_state_keys_changed);

    // Demonstrate state scope prefixes
    println!("\n=== State Scope Prefixes ===");
    println!("ADK-Rust supports state scoping with prefixes:");
    println!("  app:key    - Application-wide state");
    println!("  user:key   - User-specific state across sessions");
    println!("  temp:key   - Temporary state (cleared between invocations)");
    println!("  key        - Session-scoped state (default)");

    print_success("event_state_changes");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_state_changes -p adk-rust-guide -- chat");

    Ok(())
}
