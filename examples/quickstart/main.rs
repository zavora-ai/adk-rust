use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from .env if present.
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("weather_time_agent")
        .description("Agent to answer questions about the time and weather in a city.")
        .instruction("Your SOLE purpose is to answer questions about the current time and weather in a specific city. You MUST refuse to answer any questions unrelated to time or weather.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let app_name = "quickstart".to_string();
    let user_id = "user1".to_string();

    adk_cli::console::run_console(Arc::new(agent), app_name, user_id).await?;

    Ok(())
}
