use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();
    
    // Get API key from environment
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Build your agent
    let agent = LlmAgentBuilder::new("my_assistant")
        .description("A helpful AI assistant")
        .instruction("You are a friendly and helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))  // Add search capability
        .build()?;

    // Run the agent with the CLI launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}