//! Filtered browser tools example
//!
//! Demonstrates selective tool loading for specific use cases.
//!
//! Run: cargo run --bin filtered

use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let browser = Arc::new(BrowserSession::new(BrowserConfig::new()));

    // Create minimal toolset for web scraping (read-only)
    let toolset = BrowserToolset::new(browser.clone())
        .with_navigation(true)   // navigate, back, forward, refresh
        .with_extraction(true)   // extract_text, extract_links, page_info
        .with_interaction(false) // no clicking/typing
        .with_wait(true)         // wait for elements
        .with_screenshot(true)   // screenshots
        .with_js(true)           // scroll, evaluate_js
        .with_cookies(false)
        .with_windows(false)
        .with_frames(false)
        .with_actions(false);

    let tools = toolset.all_tools();
    println!("✅ Scraping toolset: {} tools (read-only)", tools.len());

    let mut builder = LlmAgentBuilder::new("scraper_agent")
        .instruction("You are a web scraper. Navigate to pages and extract information. Do not interact with forms.")
        .model(Arc::new(model));

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    println!("✅ Scraper agent ready");
    println!("   Note: Requires WebDriver at localhost:4444");

    Launcher::new(Arc::new(agent)).run().await?;

    browser.stop().await.ok();

    Ok(())
}
