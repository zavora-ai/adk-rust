use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_ui::{UiToolset, a2ui::A2UI_AGENT_PROMPT};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let ui_tools = UiToolset::all_tools();

    let mut builder = LlmAgentBuilder::new("a2ui_demo")
        .description("Agent demonstrating A2UI v0.9 component generation")
        .instruction(A2UI_AGENT_PROMPT)
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?));

    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    println!("=== A2UI v0.9 Demo ===");
    println!("This agent generates proper A2UI components with nested structure.");
    println!();
    println!("Try: \"Create a welcome screen\"");
    println!();

    adk_cli::console::run_console(Arc::new(agent), "a2ui_demo".to_string(), "user1".to_string())
        .await?;

    Ok(())
}
