use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_model::gemini::GeminiModel;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Step 1: Analyze the topic
    let analyzer = LlmAgentBuilder::new("analyzer")
        .description("Analyzes topics")
        .instruction("Analyze the given topic and identify key points.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .build()?;

    // Step 2: Expand on the analysis
    let expander = LlmAgentBuilder::new("expander")
        .description("Expands on analysis")
        .instruction("Take the analysis and expand on each key point with details.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?))
        .build()?;

    // Step 3: Summarize
    let summarizer = LlmAgentBuilder::new("summarizer")
        .description("Summarizes content")
        .instruction("Create a concise summary of the expanded analysis.")
        .model(Arc::new(model))
        .build()?;

    let sequential = SequentialAgent::new(
        "sequential_workflow",
        vec![Arc::new(analyzer), Arc::new(expander), Arc::new(summarizer)],
    );

    adk_cli::console::run_console(
        Arc::new(sequential),
        "sequential_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
