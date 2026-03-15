use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_ui::{UiToolset, a2ui::A2UI_AGENT_PROMPT};
use anyhow::Result;
use std::sync::Arc;

const INSTRUCTION: &str = r#"
You are an event RSVP assistant with working UI flows.

Use render_page to show event details and agenda.
Use render_screen to collect RSVP details (name, guests, dietary, sessions).
Ensure A2UI components include root id "root" and valid Button actions.

After submission, render a confirmation screen and a calendar link button.
"#;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let ui_tools = UiToolset::all_tools();

    let mut builder = LlmAgentBuilder::new("ui_working_events")
        .description("Event RSVP agent with working UI flows")
        .instruction(format!(
            "{}

{}",
            A2UI_AGENT_PROMPT, INSTRUCTION
        ))
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-3-flash-preview")?));

    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;
    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8083);

    println!("=== Working UI: Event RSVP ===");
    println!("Server running on http://localhost:{}", port);
    println!("Open http://localhost:5173 and select 'Events' from the dropdown");

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
