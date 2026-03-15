//! OpenAI Server Example
//!
//! This example demonstrates running an ADK server with OpenAI as the backend.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_server --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful AI assistant powered by OpenAI.")
        .instruction("You are a helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .build()?;

    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));

    let port = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    println!("Starting OpenAI ADK server on port {}", port);
    println!("\nEndpoints:");
    println!("  Health:   GET  http://localhost:{}/api/health", port);
    println!("  Sessions: POST http://localhost:{}/api/sessions", port);
    println!("  Web UI:   http://localhost:{}/ui/", port);
    println!("\nPress Ctrl+C to stop the server\n");

    adk_cli::serve::run_serve(agent_loader, port).await?;

    Ok(())
}
