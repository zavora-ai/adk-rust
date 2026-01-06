//! Logging Callbacks Example
//!
//! Demonstrates using callbacks for logging agent interactions.
//!
//! Run:
//!   cd doc-test/callbacks/callbacks_test
//!   GOOGLE_API_KEY=your_key cargo run --bin logging

use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    println!("Logging Callbacks Example");
    println!("=========================\n");

    let agent = LlmAgentBuilder::new("logged_agent")
        .model(model)
        .instruction("You are a helpful assistant. Be brief.")
        // Before agent callback - logging
        .before_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[LOG] Agent '{}' starting", ctx.agent_name());
                println!("[LOG] Session: {}", ctx.session_id());
                println!("[LOG] User: {}", ctx.user_id());
                Ok(None) // Continue execution
            })
        }))
        // After agent callback - logging
        .after_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[LOG] Agent '{}' completed", ctx.agent_name());
                Ok(None) // Keep original result
            })
        }))
        .build()?;

    // Run with console
    adk_cli::console::run_console(
        Arc::new(agent),
        "callbacks_demo".to_string(),
        "user".to_string(),
    )
    .await?;

    Ok(())
}
