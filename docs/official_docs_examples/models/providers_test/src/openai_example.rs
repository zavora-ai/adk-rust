//! OpenAI Provider Example
//! Validates: providers.md - OpenAI section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("🔥 OpenAI Provider Example");
    println!("==========================\n");
    
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5-mini"))?;

    println!("✅ Model: gpt-5-mini");
    println!("📝 Key highlights:");
    println!("   • Industry standard");
    println!("   • Excellent tool/function calling");
    println!("   • Best documentation & ecosystem\n");

    let agent = LlmAgentBuilder::new("openai_assistant")
        .description("OpenAI-powered assistant")
        .instruction("You are a helpful assistant powered by OpenAI GPT-4o. Be concise.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
