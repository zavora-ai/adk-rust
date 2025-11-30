//! Validates: docs/official_docs/events/events.md
//!
//! This example demonstrates how to identify different types of events.
//! It shows user messages, agent responses, tool calls, and tool results.
//!
//! Run modes:
//!   cargo run --example event_types -p adk-rust-guide              # Validation mode
//!   cargo run --example event_types -p adk-rust-guide -- chat      # Interactive console

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

    // Create a simple calculator tool to demonstrate tool call events
    let calculator = FunctionTool::new(
        "calculator",
        "Performs basic arithmetic operations. Parameters: operation (add/subtract/multiply/divide), a (number), b (number)",
        |_ctx, args| {
            Box::pin(async move {
                let operation = args["operation"].as_str().unwrap_or("");
                let a = args["a"].as_f64().unwrap_or(0.0);
                let b = args["b"].as_f64().unwrap_or(0.0);

                let result = match operation {
                    "add" => a + b,
                    "subtract" => a - b,
                    "multiply" => a * b,
                    "divide" if b != 0.0 => a / b,
                    _ => return Err(AdkError::Tool("Invalid operation or division by zero".into())),
                };

                Ok(json!({ "result": result }))
            })
        },
    );

    let agent = Arc::new(
        LlmAgentBuilder::new("type_demo")
            .model(Arc::new(model))
            .instruction("You are a helpful assistant. Use the calculator tool for math.")
            .tool(Arc::new(calculator))
            .build()?,
    );

    if is_interactive_mode() {
        Launcher::new(agent).run().await?;
        return Ok(());
    }

    print_validating("events/events.md");

    println!("\n=== Event Types Demonstration ===\n");

    let session_service = Arc::new(InMemorySessionService::new());

    session_service
        .create(CreateRequest {
            app_name: "type_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("type_session".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "type_demo".to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;

    // Send a message that will trigger a tool call
    let user_input = Content::new("user").with_text("What is 15 multiplied by 7?");

    let mut stream = runner
        .run(
            "user_123".to_string(),
            "type_session".to_string(),
            user_input,
        )
        .await?;

    println!("Processing events and identifying types...\n");

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                println!("─────────────────────────────────────");
                println!("Event ID: {}", event.id);
                println!("Author: {}", event.author);

                // Identify event type
                let event_type = identify_event_type(&event);
                println!("Type: {}", event_type);

                // Show relevant details based on type
                match event_type.as_str() {
                    "User Message" => {
                        if let Some(content) = &event.llm_response.content {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    println!("Message: {}", text);
                                }
                            }
                        }
                    }
                    "Agent Response" => {
                        if let Some(content) = &event.llm_response.content {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    println!("Response: {}", text);
                                }
                            }
                        }
                    }
                    "Tool Call" => {
                        if let Some(content) = &event.llm_response.content {
                            for part in &content.parts {
                                if let Part::FunctionCall { name, args } = part {
                                    println!("Tool: {}", name);
                                    println!("Arguments: {}", args);
                                }
                            }
                        }
                    }
                    "Tool Result" => {
                        if let Some(content) = &event.llm_response.content {
                            for part in &content.parts {
                                if let Part::FunctionResponse { name, response } = part {
                                    println!("Tool: {}", name);
                                    println!("Result: {}", response);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    println!("\n─────────────────────────────────────");

    // Retrieve and categorize all events
    let session = session_service
        .get(GetRequest {
            app_name: "type_demo".to_string(),
            user_id: "user_123".to_string(),
            session_id: "type_session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let events = session.events();

    println!("\n=== Event Type Summary ===");
    let mut type_counts: HashMap<String, usize> = HashMap::new();

    for i in 0..events.len() {
        if let Some(event) = events.at(i) {
            let event_type = identify_session_event_type(event);
            *type_counts.entry(event_type).or_insert(0) += 1;
        }
    }

    for (event_type, count) in &type_counts {
        println!("{}: {} event(s)", event_type, count);
    }

    println!("\nTotal events: {}", events.len());

    print_success("event_types");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example event_types -p adk-rust-guide -- chat");

    Ok(())
}

/// Helper function to identify event type (works with adk_core::Event from stream)
fn identify_event_type(event: &adk_rust::Event) -> String {
    // Check author first
    if event.author == "user" {
        return "User Message".to_string();
    }

    // Check content for specific patterns
    if let Some(content) = &event.llm_response.content {
        for part in &content.parts {
            match part {
                Part::FunctionCall { .. } => return "Tool Call".to_string(),
                Part::FunctionResponse { .. } => return "Tool Result".to_string(),
                Part::Text { .. } => {
                    // Agent text response
                    if event.author != "user" {
                        return "Agent Response".to_string();
                    }
                }
                _ => {}
            }
        }
    }

    // Check for state/control events
    if !event.actions.state_delta.is_empty() {
        return "State Update".to_string();
    }

    if event.actions.transfer_to_agent.is_some() {
        return "Agent Transfer".to_string();
    }

    if event.actions.escalate {
        return "Escalation".to_string();
    }

    "Other".to_string()
}

/// Helper function to identify event type (works with adk_session::Event from session)
fn identify_session_event_type(event: &adk_rust::session::Event) -> String {
    // Check author first
    if event.author == "user" {
        return "User Message".to_string();
    }

    // Check content for specific patterns (session events use llm_response.content)
    if let Some(content) = &event.llm_response.content {
        for part in &content.parts {
            match part {
                Part::FunctionCall { .. } => return "Tool Call".to_string(),
                Part::FunctionResponse { .. } => return "Tool Result".to_string(),
                Part::Text { .. } => {
                    // Agent text response
                    if event.author != "user" {
                        return "Agent Response".to_string();
                    }
                }
                _ => {}
            }
        }
    }

    // Check for state/control events
    if !event.actions.state_delta.is_empty() {
        return "State Update".to_string();
    }

    if event.actions.transfer_to_agent.is_some() {
        return "Agent Transfer".to_string();
    }

    if event.actions.escalate {
        return "Escalation".to_string();
    }

    "Other".to_string()
}
