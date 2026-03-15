use adk_agent::LlmAgentBuilder;
use adk_model::ollama::{OllamaConfig, OllamaModel};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK Ollama Example");
    println!("---------------------");

    // Create Ollama model
    // Assumes Ollama is running on localhost:11434
    // You can change the model name here (e.g. "llama3.2", "mistral")
    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    println!("Connecting to Ollama with model: {}", model_name);

    let config = OllamaConfig::new(&model_name);
    let model = OllamaModel::new(config)?;

    // Create agent
    let agent = LlmAgentBuilder::new("local-assistant")
        .description("A helpful local assistant")
        .model(Arc::new(model))
        .instruction("You are a helpful assistant running locally via Ollama.")
        .build()?;

    // Run agent in console mode
    adk_cli::console::run_console(Arc::new(agent), "ollama_basic".to_string(), "user1".to_string())
        .await?;

    Ok(())
}
