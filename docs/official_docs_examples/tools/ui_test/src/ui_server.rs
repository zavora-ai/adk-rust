//! UI Server with SSE streaming
//!
//! Run: cargo run --bin ui_server

use adk_rust::prelude::*;
use adk_rust::SingleAgentLoader;
use adk_ui::{UiToolset, UI_AGENT_PROMPT};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Get all UI tools
    let ui_tools = UiToolset::all_tools();

    // Build agent with UI tools
    let mut builder = LlmAgentBuilder::new("ui_demo")
        .description("An agent that uses dynamic UI components")
        .instruction(UI_AGENT_PROMPT)
        .model(Arc::new(model));

    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;
    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    println!("=== ADK UI Server ===");
    println!("Server running on http://localhost:{}", port);
    println!();
    println!("API Endpoints:");
    println!("  POST /api/apps/{{app}}/users/{{user}}/sessions - Create session");
    println!("  POST /api/run/{{app}}/{{user}}/{{session}} - Run agent (SSE)");

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
