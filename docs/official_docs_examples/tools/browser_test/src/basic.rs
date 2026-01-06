//! Basic browser tools example
//!
//! Demonstrates BrowserToolset with an LLM agent.
//! Requires WebDriver running at localhost:4444
//!
//! Run: cargo run --bin basic

use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Configure browser
    let config = BrowserConfig::new()
        .webdriver_url("http://localhost:4444")
        .headless(true)
        .viewport(1920, 1080);

    let browser = Arc::new(BrowserSession::new(config));

    // Create toolset with all 46 tools
    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();

    println!("✅ BrowserToolset created with {} tools", tools.len());

    // Build agent with browser tools
    let mut builder = LlmAgentBuilder::new("browser_agent")
        .instruction("You are a web automation assistant. Use browser tools to navigate and extract information.")
        .model(Arc::new(model));

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    println!("✅ Browser agent ready");
    println!("   Note: Requires WebDriver at localhost:4444");
    println!("   Start with: docker run -d -p 4444:4444 selenium/standalone-chrome");

    Launcher::new(Arc::new(agent)).run().await?;

    // Cleanup
    browser.stop().await.ok();

    Ok(())
}
