use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_ui::{a2ui::A2UI_AGENT_PROMPT, UiToolset};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Get all UI tools
    let ui_tools = UiToolset::all_tools();

    // Create an agent with UI rendering capabilities
    // Uses the A2UI v0.9 prompt for proper nested component generation
    let mut builder = LlmAgentBuilder::new("ui_demo")
        .description("An agent that uses A2UI v0.9 components to interact with users")
        .instruction(A2UI_AGENT_PROMPT)
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?));

    // Add each tool individually
    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let ui_agent = builder.build()?;
    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(ui_agent)));

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    println!("=== ADK UI Server ===");
    println!("Server running on http://localhost:{}", port);
    println!();
    println!("API Endpoints:");
    println!("  POST /api/apps/{{app}}/users/{{user}}/sessions - Create session");
    println!("  POST /api/run/{{app}}/{{user}}/{{session}} - Run agent (SSE)");
    println!();
    println!("Open the React client at http://localhost:5173 to interact with the agent.");
    println!();

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
