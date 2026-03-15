use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Agent to answer questions about weather.")
        .instruction("Answer questions about weather using Google Search.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    println!("Starting ADK server on port {}", port);
    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
