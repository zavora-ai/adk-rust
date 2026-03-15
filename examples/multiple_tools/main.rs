use adk_agent::LlmAgentBuilder;
use adk_core::ToolContext;
use adk_model::gemini::GeminiModel;
use adk_tool::{FunctionTool, GoogleSearchTool};
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Arc;

async fn generate_poem(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value, adk_core::AdkError> {
    let line_count = args["line_count"].as_u64().unwrap_or(3) as usize;
    let poem = format!("{}\n", "A line of a poem,".repeat(line_count));
    Ok(json!({ "poem": poem }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Search agent with GoogleSearch tool
    let search_agent = LlmAgentBuilder::new("search_agent")
        .description("Does google search")
        .instruction("You're a specialist in Google Search.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    // Poem agent with custom function tool
    let poem_tool =
        FunctionTool::new("poem", "Returns a poem with specified number of lines", generate_poem);

    let poem_agent = LlmAgentBuilder::new("poem_agent")
        .description("Returns poems")
        .instruction("You return poems using the poem tool.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(poem_tool))
        .build()?;

    // Root agent orchestrating sub-agents
    let root_agent = LlmAgentBuilder::new("root_agent")
        .description("Can do google search and generate poems")
        .instruction("Answer questions about weather based on google search. For poems, generate them with the poem agent.")
        .model(Arc::new(model))
        .sub_agent(Arc::new(search_agent))
        .sub_agent(Arc::new(poem_agent))
        .build()?;

    adk_cli::console::run_console(
        Arc::new(root_agent),
        "multiple_tools_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
