//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates event handling and inspection.
//!
//! Run modes:
//!   cargo run --example event_inspection -p adk-rust-guide              # Validation mode
//!   cargo run --example event_inspection -p adk-rust-guide -- chat      # Interactive console

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

    // Create a simple agent with event logging callback
    let agent = Arc::new(
        LlmAgentBuilder::new("event_inspector")
            .model(Arc::new(model))
            .instruction("You are a helpful assistant. Keep responses brief.")
            .after_callback(Box::new(|ctx| {
                Box::pin(async move {
                    // Log event information after each agent response
                    println!("\n[EVENT LOG]");
                    println!("  Agent: {}", ctx.agent_name());
                    println!("  Session: {}", ctx.session_id());
                    println!("  Invocation: {}", ctx.invocation_id());
                    println!("  User: {}", ctx.user_id());
                    println!();
                    
                    Ok(None)
                })
            }))
            .build()?,
    );

    // Check if we should run in interactive mode
    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    // Otherwise, run in validation mode
    print_validating("events/events.md");

    // =========================================================================
    // 1. Create a session and agent
    // =========================================================================
    println!("\n--- Setting Up Session and Agent ---");

    let session_service = Arc::new(InMemorySessionService::new());
    
    let mut initial_state = HashMap::new();
    initial_state.insert("user_name".to_string(), json!("Alice"));
    initial_state.insert("conversation_count".to_string(), json!(0));

    let session = session_service
        .create(CreateRequest {
            app_name: "event_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("demo_session".to_string()),
            state: initial_state,
        })
        .await?;

    println!("Created session: {}", session.id());
    println!("Initial events: {}", session.events().len());

    // =========================================================================
    // 2. Run agent to generate events
    // =========================================================================
    println!("\n--- Running Agent to Generate Events ---");

    let runner = Runner::new(RunnerConfig {
        app_name: "event_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;

    let input = Content::new("user").with_text("Hello! What's 2+2?");

    let mut stream = runner
        .run("user_123".to_string(), "demo_session".to_string(), input)
        .await?;

    // Collect the response
    print!("Agent response: ");
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            print!("{}", text);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("\nError: {}", e);
                break;
            }
        }
    }
    println!();

    // =========================================================================
    // 3. Retrieve session and inspect events
    // =========================================================================
    println!("\n--- Inspecting Events ---");

    let updated_session = session_service
        .get(GetRequest {
            app_name: "event_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "demo_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let events = updated_session.events();
    println!("Total events after agent run: {}", events.len());
    println!();

    // =========================================================================
    // 4. Examine each event in detail
    // =========================================================================
    println!("--- Event Details ---");

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            println!("\n[Event {}]", i + 1);
            println!("  ID: {}", event.id);
            println!("  Timestamp: {}", event.timestamp);
            println!("  Invocation ID: {}", event.invocation_id);
            println!("  Author: {}", event.author);

            // Check content from llm_response
            if let Some(content) = &event.llm_response.content {
                let mut text_parts = Vec::new();
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        text_parts.push(text.as_str());
                    }
                }
                let text = text_parts.join(" ");
                if !text.is_empty() {
                    let preview = if text.len() > 60 {
                        format!("{}...", &text[..60])
                    } else {
                        text.to_string()
                    };
                    println!("  Content: {}", preview);
                } else {
                    println!("  Content: (empty)");
                }
            } else {
                println!("  Content: (none)");
            }

            // Check state delta
            if !event.actions.state_delta.is_empty() {
                println!("  State Changes:");
                for (key, value) in &event.actions.state_delta {
                    println!("    {} = {}", key, value);
                }
            }

            // Check artifact delta
            if !event.actions.artifact_delta.is_empty() {
                println!("  Artifact Changes:");
                for (name, version) in &event.actions.artifact_delta {
                    println!("    {} (version {})", name, version);
                }
            }

            // Check for agent transfers
            if let Some(target) = &event.actions.transfer_to_agent {
                println!("  Transfer to: {}", target);
            }

            // Check for escalation
            if event.actions.escalate {
                println!("  Escalated: true");
            }

            // Check skip summarization
            if event.actions.skip_summarization {
                println!("  Skip Summarization: true");
            }
        }
    }

    // =========================================================================
    // 5. Demonstrate event grouping by invocation_id
    // =========================================================================
    println!("\n--- Event Grouping by Invocation ---");

    let mut invocation_groups: HashMap<String, Vec<usize>> = HashMap::new();

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            invocation_groups
                .entry(event.invocation_id.clone())
                .or_insert_with(Vec::new)
                .push(i);
        }
    }

    println!("Found {} unique invocations:", invocation_groups.len());
    for (inv_id, indices) in &invocation_groups {
        println!("  Invocation {}: {} events", inv_id, indices.len());
        for &idx in indices {
            if let Some(event) = events.at(idx) {
                println!("    - Event {} by {}", idx + 1, event.author);
            }
        }
    }

    // =========================================================================
    // 6. Demonstrate conversation history formation
    // =========================================================================
    println!("\n--- Conversation History ---");

    println!("Chronological conversation flow:");
    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            if let Some(content) = &event.llm_response.content {
                let mut text_parts = Vec::new();
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        text_parts.push(text.as_str());
                    }
                }
                let text = text_parts.join(" ");
                if !text.is_empty() {
                    let preview = if text.len() > 80 {
                        format!("{}...", &text[..80])
                    } else {
                        text.to_string()
                    };
                    println!("  [{}] {}: {}", 
                        event.timestamp.format("%H:%M:%S"),
                        event.author,
                        preview
                    );
                }
            }
        }
    }

    // =========================================================================
    // 7. Verify event properties
    // =========================================================================
    println!("\n--- Verifying Event Properties ---");

    // Verify we have at least user and agent events
    let has_user_event = (0..events.len()).any(|i| {
        events
            .at(i)
            .map(|e| e.author == "user")
            .unwrap_or(false)
    });

    let has_agent_event = (0..events.len()).any(|i| {
        events
            .at(i)
            .map(|e| e.author == "event_inspector")
            .unwrap_or(false)
    });

    assert!(has_user_event, "Should have at least one user event");
    println!("✓ User event found");

    assert!(has_agent_event, "Should have at least one agent event");
    println!("✓ Agent event found");

    // Verify events are ordered chronologically
    let mut is_chronological = true;
    for i in 1..events.len() {
        if let (Some(prev), Some(curr)) = (events.at(i - 1), events.at(i)) {
            if curr.timestamp < prev.timestamp {
                is_chronological = false;
                break;
            }
        }
    }
    assert!(is_chronological, "Events should be in chronological order");
    println!("✓ Events are chronologically ordered");

    // Verify all events have unique IDs
    let mut event_ids = std::collections::HashSet::new();
    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            event_ids.insert(event.id.clone());
        }
    }
    assert_eq!(
        event_ids.len(),
        events.len(),
        "All event IDs should be unique"
    );
    println!("✓ All event IDs are unique");

    print_success("event_inspection");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_inspection -p adk-rust-guide -- chat");

    Ok(())
}
