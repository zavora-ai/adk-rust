//! Instruction Templating - Demonstrates {var} syntax with session state
//!
//! This example shows how to use template variables in instructions that get
//! replaced with values from session state at runtime.

use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Agent with templated instruction
    let agent = LlmAgentBuilder::new("personalized")
        .instruction(
            "You are helping {user_name}. Their role is {user_role}. \
                     Tailor your responses to their expertise level.",
        )
        .model(Arc::new(model))
        .build()?;

    // Create session service and runner
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "templating_demo".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    // Create session with state variables
    let mut state = HashMap::new();
    state.insert("user_name".to_string(), json!("Alice"));
    state.insert("user_role".to_string(), json!("Senior Developer"));

    let session = session_service
        .create(CreateRequest {
            app_name: "templating_demo".to_string(),
            user_id: "user123".to_string(),
            session_id: None,
            state,
        })
        .await?;

    println!("📝 Instruction Templating Demo");
    println!();
    println!("Template: 'You are helping {{user_name}}. Their role is {{user_role}}.'");
    println!("Becomes:  'You are helping Alice. Their role is Senior Developer.'");
    println!();

    // Run the agent with templated instruction
    let mut response_stream = runner
        .run(
            "user123".to_string(),
            session.id().to_string(),
            Content::new("user").with_text("Explain async/await in Rust"),
        )
        .await?;

    // Print the response
    while let Some(event) = response_stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{}", text);
                }
            }
        }
    }
    println!();

    Ok(())
}
