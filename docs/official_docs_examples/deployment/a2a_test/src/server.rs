//! A2A Server Example
//!
//! Exposes an agent via A2A protocol.
//!
//! Run:
//!   cd doc-test/deployment/a2a_test
//!   GOOGLE_API_KEY=your_key cargo run --bin server

use adk_cli::launcher::SingleAgentLoader;
use adk_rust::prelude::*;
use adk_server::{ServerConfig, create_app_with_a2a};
use adk_session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    dotenvy::dotenv().ok();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY environment variable not set");
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create agent to expose via A2A
    let agent = LlmAgentBuilder::new("math_helper")
        .description("Helps with math questions")
        .instruction("You are a math assistant. Answer math questions concisely.")
        .model(model)
        .build()?;

    // Create server config
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::new(agent))),
        Arc::new(InMemorySessionService::new()),
    );

    // Create app with A2A support
    let app = create_app_with_a2a(config, Some("http://localhost:8090"));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8090").await?;

    println!("🚀 A2A Server starting on http://localhost:8090");
    println!("📋 Agent card: http://localhost:8090/.well-known/agent.json");
    println!("📡 A2A endpoint: http://localhost:8090/a2a");
    println!("Press Ctrl+C to stop\n");

    axum::serve(listener, app).await?;

    Ok(())
}
