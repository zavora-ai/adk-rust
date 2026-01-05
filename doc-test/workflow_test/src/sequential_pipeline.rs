//! Sequential Pipeline - Research → Analyze → Summarize

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Step 1: Research agent gathers information
    let researcher = LlmAgentBuilder::new("researcher")
        .instruction("Research the given topic. List 3-5 key facts. Be factual and concise.")
        .model(model.clone())
        .output_key("research")
        .build()?;

    // Step 2: Analyzer agent identifies patterns
    let analyzer = LlmAgentBuilder::new("analyzer")
        .instruction("Based on the research above, identify 2-3 key insights. \
                     What's the bigger picture?")
        .model(model.clone())
        .output_key("analysis")
        .build()?;

    // Step 3: Summarizer creates final output
    let summarizer = LlmAgentBuilder::new("summarizer")
        .instruction("Create a brief executive summary. Keep it under 100 words.")
        .model(model.clone())
        .build()?;

    // Create the sequential pipeline
    let pipeline = SequentialAgent::new(
        "research_pipeline",
        vec![Arc::new(researcher), Arc::new(analyzer), Arc::new(summarizer)],
    ).with_description("Research → Analyze → Summarize");

    println!("📋 Sequential Pipeline: Research → Analyze → Summarize");
    println!();
    println!("Try: 'Tell me about Rust programming language'");
    println!();

    Launcher::new(Arc::new(pipeline)).run().await?;
    Ok(())
}
