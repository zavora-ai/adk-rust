use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_ui::UiToolset;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Get all UI tools
    let ui_tools = UiToolset::all_tools();

    // Create an agent with UI rendering capabilities
    let mut builder = LlmAgentBuilder::new("ui_agent")
        .description("An agent that uses dynamic UI components to interact with users")
        .instruction(
            r#"
            You are a helpful assistant that uses UI components to interact with users.
            
            When users need to provide information (like registration, settings, or surveys), 
            use the render_form tool to create a form.
            
            When showing results or information, use render_card.
            
            When asking for confirmation before important actions, use render_confirm.
            
            For notifications or status updates, use render_alert.
            
            Always prefer UI components over plain text responses for structured interactions.
        "#,
        )
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?));

    // Add each tool individually
    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let ui_agent = builder.build()?;

    let app_name = "ui_demo".to_string();
    let user_id = "user1".to_string();

    println!("=== ADK UI Agent Demo ===");
    println!("This agent can generate dynamic UI components.");
    println!();
    println!("Try these prompts:");
    println!("  - \"I want to register\"");
    println!("  - \"Show me my profile\"");
    println!("  - \"Delete my account\"");
    println!();

    adk_cli::console::run_console(Arc::new(ui_agent), app_name, user_id).await?;

    Ok(())
}
