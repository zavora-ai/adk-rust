//! Groq Provider Example
//! Validates: providers.md - Groq section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("⚡ Groq Provider Example");
    println!("========================\n");
    
    let api_key = std::env::var("GROQ_API_KEY")?;
    
    // Using the convenience method
    let model = GroqClient::llama70b(&api_key)?;

    println!("✅ Model: llama-3.3-70b-versatile");
    println!("📝 Key highlights:");
    println!("   • Fastest inference (10x faster)");
    println!("   • LPU technology");
    println!("   • Competitive pricing\n");

    let agent = LlmAgentBuilder::new("groq_assistant")
        .description("Groq-powered assistant")
        .instruction("You are a helpful assistant powered by Groq. Be concise and fast.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
