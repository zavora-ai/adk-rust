//! README Browser Automation snippet validation

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create browser session
    let config = BrowserConfig::new().webdriver_url("http://localhost:4444");
    let session = Arc::new(BrowserSession::new(config));

    // Get all browser tools
    let toolset = BrowserToolset::new(session.clone());
    let tools = toolset.all_tools();

    // Add to agent
    let mut builder = LlmAgentBuilder::new("web_agent")
        .model(model)
        .instruction("Browse the web and extract information.");

    for tool in tools {
        builder = builder.tool(tool);
    }

    let _agent = builder.build()?;

    println!("✓ Browser snippet compiles");
    Ok(())
}
