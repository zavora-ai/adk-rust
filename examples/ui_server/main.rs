use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, MultiAgentLoader, Tool};
use adk_model::gemini::GeminiModel;
use adk_ui::{UiToolset, a2ui::A2UI_AGENT_PROMPT};
use anyhow::Result;
use std::sync::Arc;

const UI_DEMO_INSTRUCTION: &str = r#"
You are a general-purpose UI assistant.

Render clear, production-style interfaces for dashboards, forms, tables, charts, alerts, and modals.
Prefer high-level tools (`render_layout`, `render_table`, `render_chart`, `render_form`, `render_card`) over plain text-only outputs.
For dashboard prompts, include at minimum:
- 3 KPI cards
- 1 table or list
- 1 chart
- 1 alert or status badge cluster
Do not satisfy dashboard prompts using text paragraphs plus buttons only.
Always return complete surfaces with stable ids and actionable controls.
"#;

const SUPPORT_INSTRUCTION: &str = r#"
You are a support intake assistant.

When the user starts, immediately render a support ticket form with:
- Title input
- Description textarea
- Priority select (Low, Medium, High)
- Submit button

Use `render_form` for intake and `render_alert` for success/error feedback.
Use render_screen with a root Column layout.
"#;

const APPOINTMENT_INSTRUCTION: &str = r#"
You are a clinic scheduling assistant that renders working UIs.

Use render_layout for overviews and render_page for supporting sections (services, hours, policies).
Use render_card for service options and render_table for schedule availability.
Use render_screen for booking flows and ensure:
- root component id "root"
- layout with Column/Row
- Button actions include action.event.name

After a booking submission, render a confirmation screen with the appointment details.
"#;

const EVENTS_INSTRUCTION: &str = r#"
You are an event RSVP assistant with working UI flows.

Always use render_table for agenda timeline rows with columns: time, session, speaker, room.
Always use render_card for featured speakers with non-empty title and description fields.
Use render_layout for page structure and venue summary sections.
Use render_screen to collect RSVP details (name, guests, dietary, sessions).
Ensure A2UI components include root id "root" and valid Button actions.
Do not compress agenda data into plain text lines.

After submission, render a confirmation screen and a calendar link button.
"#;

const FACILITIES_INSTRUCTION: &str = r#"
You are a facilities maintenance assistant.

Use render_layout for command-center style screens with alerts, KPI cards, and queues.
Use render_table for work-order lists and render_confirm for high-risk actions.
Use render_screen to intake work orders (location, issue type, urgency, contact).
Use render_page for maintenance guidelines or status summaries.
Ensure A2UI components include root id "root".

After intake, render a confirmation with next steps and an emergency contact action.
"#;

const INVENTORY_INSTRUCTION: &str = r#"
You are an inventory restock assistant.

Use render_layout for inventory command views and render_table for stock/reorder grids.
Use render_chart for trend/forecast visuals and render_card for supplier summaries.
Inventory monitor responses must include:
- a stock table with SKU, quantity, threshold, and status
- at least one alert
- at least one chart or summary card row
Use render_screen to collect restock requests (SKU, qty, priority, notes).
Use render_page for inventory summaries and reorder recommendations.
Ensure A2UI components include a root id "root" and explicit child ids.

On submit, show a confirmation card or alert with the request summary.
"#;

fn full_instruction(extra: &str) -> String {
    format!("{A2UI_AGENT_PROMPT}\n\n{extra}")
}

fn build_ui_agent(
    name: &str,
    description: &str,
    instruction: &str,
    api_key: &str,
    model_name: &str,
    ui_tools: &[Arc<dyn Tool>],
) -> Result<Arc<dyn Agent>> {
    let mut builder = LlmAgentBuilder::new(name)
        .description(description)
        .instruction(full_instruction(instruction))
        .model(Arc::new(GeminiModel::new(api_key, model_name)?));

    for tool in ui_tools.iter().cloned() {
        builder = builder.tool(tool);
    }

    Ok(Arc::new(builder.build()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");
    let model_name =
        std::env::var("UI_DEMO_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());

    let ui_tools = UiToolset::all_tools();

    let ui_demo = build_ui_agent(
        "ui_demo",
        "General purpose multi-surface demo agent",
        UI_DEMO_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;
    let ui_working_support = build_ui_agent(
        "ui_working_support",
        "Support intake agent with working UI flows",
        SUPPORT_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;
    let ui_working_appointment = build_ui_agent(
        "ui_working_appointment",
        "Appointment scheduling agent with working UI flows",
        APPOINTMENT_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;
    let ui_working_events = build_ui_agent(
        "ui_working_events",
        "Event RSVP agent with working UI flows",
        EVENTS_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;
    let ui_working_facilities = build_ui_agent(
        "ui_working_facilities",
        "Facilities maintenance agent with working UI flows",
        FACILITIES_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;
    let ui_working_inventory = build_ui_agent(
        "ui_working_inventory",
        "Inventory restock agent with working UI flows",
        INVENTORY_INSTRUCTION,
        &api_key,
        &model_name,
        &ui_tools,
    )?;

    let agent_loader = Arc::new(MultiAgentLoader::new(vec![
        ui_demo,
        ui_working_support,
        ui_working_appointment,
        ui_working_events,
        ui_working_facilities,
        ui_working_inventory,
    ])?);

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    println!("=== ADK UI Aggregated Server ===");
    println!("Server running on http://localhost:{}", port);
    println!("Model: {}", model_name);
    println!();
    println!("Loaded apps:");
    println!("  - ui_demo");
    println!("  - ui_working_support");
    println!("  - ui_working_appointment");
    println!("  - ui_working_events");
    println!("  - ui_working_facilities");
    println!("  - ui_working_inventory");
    println!();
    println!("React client: http://localhost:5173");
    println!();

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
