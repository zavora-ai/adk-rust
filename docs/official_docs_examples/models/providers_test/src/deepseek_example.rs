//! DeepSeek Provider Example
//! Validates: providers.md - DeepSeek section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("💭 DeepSeek Provider Example");
    println!("============================\n");
    
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    
    // Using the convenience method
    let model = DeepSeekClient::chat(&api_key)?;

    println!("✅ Model: deepseek-chat");
    println!("📝 Key highlights:");
    println!("   • Thinking mode (with reasoner)");
    println!("   • Very cost-effective (10x cheaper)");
    println!("   • Strong at math and coding\n");
    
    println!("💡 Tip: Use DeepSeekClient::reasoner() for chain-of-thought\n");

    let agent = LlmAgentBuilder::new("deepseek_assistant")
        .description("DeepSeek-powered assistant")
        .instruction("You are a helpful assistant powered by DeepSeek. Be concise.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
