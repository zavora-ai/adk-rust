//! UI Agent Console Demo
//!
//! Demonstrates UiToolset with an LLM agent for dynamic UI generation.
//!
//! Run: cargo run --bin ui_agent

use adk_rust::prelude::*;
use adk_ui::UiToolset;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Get all 10 UI tools
    let ui_tools = UiToolset::all_tools();

    // Build agent with UI tools
    let mut builder = LlmAgentBuilder::new("ui_agent")
        .description("An agent that uses dynamic UI components")
        .instruction(r#"
You are a helpful assistant that uses UI components to interact with users.

When users need to provide information (like registration, settings, or surveys), 
use the render_form tool to create a form.

When showing results or information, use render_card.

When asking for confirmation before important actions, use render_confirm.

For notifications or status updates, use render_alert.

Always prefer UI components over plain text responses for structured interactions.
"#)
        .model(Arc::new(model));

    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    println!("=== ADK UI Agent Demo ===");
    println!("This agent can generate dynamic UI components.");
    println!();
    println!("Try these prompts:");
    println!("  - \"I want to register\"");
    println!("  - \"Show me my profile\"");
    println!("  - \"Delete my account\"");
    println!();

    adk_cli::console::run_console(Arc::new(agent), "ui_demo".to_string(), "user1".to_string()).await?;

    Ok(())
}
