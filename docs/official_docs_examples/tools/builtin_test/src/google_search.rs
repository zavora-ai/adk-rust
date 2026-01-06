//! GoogleSearchTool example - web search via Gemini grounding
//!
//! Run: cargo run --bin google_search

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("research_agent")
        .instruction("You are a research assistant. Use google_search to find current information.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    println!("✅ Research agent with Google Search");
    println!("   Try: 'What is the current weather in Tokyo?'");
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
