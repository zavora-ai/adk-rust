//! README Dynamic UI Generation snippet validation

use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_ui::{UI_AGENT_PROMPT, UiToolset};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Get all UI tools
    let tools = UiToolset::all_tools();

    // Build agent with tools (using loop pattern)
    let mut builder =
        LlmAgentBuilder::new("ui_assistant").model(model).instruction(UI_AGENT_PROMPT);

    for tool in tools {
        builder = builder.tool(tool);
    }

    let _agent = builder.build()?;

    println!("✓ UI snippet compiles");
    Ok(())
}
