use adk_agent::LlmAgentBuilder;
use adk_core::MultiAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let weather_agent = LlmAgentBuilder::new("weather_agent")
        .description("Weather information agent")
        .instruction("Provide weather information for cities")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let research_agent = LlmAgentBuilder::new("research_agent")
        .description("Research and analysis agent")
        .instruction("Research topics and provide detailed analysis")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let summary_agent = LlmAgentBuilder::new("summary_agent")
        .description("Summarization agent")
        .instruction("Create concise summaries of information")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    let agent_loader = Arc::new(MultiAgentLoader::new(vec![
        Arc::new(weather_agent),
        Arc::new(research_agent),
        Arc::new(summary_agent),
    ])?);

    println!("Multi-agent web server starting on http://127.0.0.1:8080");
    println!("Available agents: weather_agent, research_agent, summary_agent");

    adk_cli::serve::run_serve(agent_loader, 8080).await?;

    Ok(())
}
