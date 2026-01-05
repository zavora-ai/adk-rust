//! Parallel Analysis - Technical | Business | UX perspectives

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Three analysts with VERY distinct personas and focus areas
    let technical = LlmAgentBuilder::new("technical_analyst")
        .instruction("You are a senior software architect. \
                     FOCUS ONLY ON: code quality, system architecture, scalability, \
                     security vulnerabilities, and tech stack choices. \
                     Start your response with '🔧 TECHNICAL:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    let business = LlmAgentBuilder::new("business_analyst")
        .instruction("You are a business strategist and MBA graduate. \
                     FOCUS ONLY ON: market opportunity, revenue model, competition, \
                     cost structure, and go-to-market strategy. \
                     Start your response with '💼 BUSINESS:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    let user_exp = LlmAgentBuilder::new("ux_analyst")
        .instruction("You are a UX researcher and designer. \
                     FOCUS ONLY ON: user journey, accessibility, pain points, \
                     visual design, and user satisfaction metrics. \
                     Start your response with '🎨 UX:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    // Create parallel agent
    let multi_analyst = ParallelAgent::new(
        "multi_perspective",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user_exp)],
    ).with_description("Technical + Business + UX in parallel");

    println!("⚡ Parallel Analysis: Technical | Business | UX");
    println!("   (All three run simultaneously!)");
    println!();
    println!("Try: 'Evaluate a mobile banking app'");
    println!();

    Launcher::new(Arc::new(multi_analyst)).run().await?;
    Ok(())
}
