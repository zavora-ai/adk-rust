use adk_agent::{LlmAgentBuilder, ParallelAgent};
use adk_model::gemini::GeminiModel;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Agent 1: Technical perspective
    let technical = LlmAgentBuilder::new("technical_analyst")
        .description("Provides technical analysis")
        .instruction("Analyze the topic from a technical perspective.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    // Agent 2: Business perspective
    let business = LlmAgentBuilder::new("business_analyst")
        .description("Provides business analysis")
        .instruction("Analyze the topic from a business perspective.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    // Agent 3: User perspective
    let user = LlmAgentBuilder::new("user_analyst")
        .description("Provides user analysis")
        .instruction("Analyze the topic from a user experience perspective.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .build()?;

    let parallel = ParallelAgent::new(
        "parallel_workflow",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user)],
    );

    adk_cli::console::run_console(
        Arc::new(parallel),
        "parallel_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
