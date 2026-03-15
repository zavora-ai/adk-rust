//! Groq Basic Chat Example
//!
//! Demonstrates basic chat with Groq's ultra-fast inference.
//!
//! Run: GROQ_API_KEY=your_key cargo run --example groq_basic --features groq

use adk_agent::LlmAgentBuilder;
use adk_model::groq::{GroqClient, GroqConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    let _ = dotenvy::dotenv();

    println!("Groq Basic Chat Example");
    println!("=======================\n");

    let api_key = std::env::var("GROQ_API_KEY").expect("GROQ_API_KEY must be set");

    // Default: llama-3.3-70b-versatile
    let model_name =
        std::env::var("GROQ_MODEL").unwrap_or_else(|_| "llama-3.3-70b-versatile".to_string());

    println!("Using model: {}\n", model_name);

    let config = GroqConfig::new(&api_key, &model_name);
    let model = GroqClient::new(config)?;

    let agent = LlmAgentBuilder::new("groq-assistant")
        .description("A helpful assistant powered by Groq")
        .instruction("You are a helpful assistant. Be concise and informative.")
        .model(Arc::new(model))
        .build()?;

    adk_cli::console::run_console(Arc::new(agent), "groq_basic".to_string(), "user1".to_string())
        .await?;

    Ok(())
}
