use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::gemini::GeminiModel;
use adk_ui::{UiToolset, a2ui::A2UI_AGENT_PROMPT};
use anyhow::Result;
use std::sync::Arc;

const INSTRUCTION: &str = r#"
You are a clinic scheduling assistant that renders working UIs.

Use render_page for overview pages (services, hours, policies).
Use render_screen for booking flows and ensure:
- root component id "root"
- layout with Column/Row
- Button actions include action.event.name

After a booking submission, render a confirmation screen with the appointment details.
"#;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let ui_tools = UiToolset::all_tools();

    let mut builder = LlmAgentBuilder::new("ui_working_appointment")
        .description("Appointment scheduling agent with working UI flows")
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

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8082);

    println!("=== Working UI: Appointment Scheduling ===");
    println!("Server running on http://localhost:{}", port);
    println!("Open http://localhost:5173 and select 'Appointments' from the dropdown");

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
