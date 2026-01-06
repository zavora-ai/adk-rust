//! Ollama Provider Example
//! Validates: providers.md - Ollama section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("🏠 Ollama Provider Example");
    println!("==========================\n");
    
    println!("📋 Prerequisites:");
    println!("   1. Install Ollama: brew install ollama");
    println!("   2. Start server: ollama serve");
    println!("   3. Pull model: ollama pull llama3.2\n");
    
    let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;

    println!("✅ Model: llama3.2");
    println!("📝 Key highlights:");
    println!("   • Completely free");
    println!("   • 100% private - data never leaves your machine");
    println!("   • Works offline\n");

    let agent = LlmAgentBuilder::new("ollama_assistant")
        .description("Ollama-powered local assistant")
        .instruction("You are a helpful assistant running locally via Ollama. Be concise.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
