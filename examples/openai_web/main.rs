//! OpenAI Multi-Agent Web Server Example
//!
//! This example demonstrates running a multi-agent web server with OpenAI.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_web --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::MultiAgentLoader;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let weather_agent = LlmAgentBuilder::new("weather_agent")
        .description("Weather information agent")
        .instruction(
            "Provide weather information for cities. Since you don't have real-time data, \
             give general climate information and typical weather patterns for locations.",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    let research_agent = LlmAgentBuilder::new("research_agent")
        .description("Research and analysis agent")
        .instruction("Research topics and provide detailed analysis based on your knowledge.")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    let summary_agent = LlmAgentBuilder::new("summary_agent")
        .description("Summarization agent")
        .instruction("Create concise summaries of information provided to you.")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?))
        .build()?;

    let agent_loader = Arc::new(MultiAgentLoader::new(vec![
        Arc::new(weather_agent),
        Arc::new(research_agent),
        Arc::new(summary_agent),
    ])?);

    println!("OpenAI Multi-agent web server starting on http://127.0.0.1:8080");
    println!("Available agents: weather_agent, research_agent, summary_agent");
    println!("\nEndpoints:");
    println!("  Health:   GET  http://localhost:8080/api/health");
    println!("  Sessions: POST http://localhost:8080/api/sessions");
    println!("  Web UI:   http://localhost:8080/ui/");
    println!("\nPress Ctrl+C to stop the server\n");

    adk_cli::serve::run_serve(agent_loader, 8080).await?;

    Ok(())
}
