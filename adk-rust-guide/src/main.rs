use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with Google Search tool
    let agent = LlmAgentBuilder::new("hello_time_agent")
        .description("Tells the current time in a specified city.")
        .instruction("You are a helpful assistant that tells the current time in a city.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    // Run with CLI support (console or web server)
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}