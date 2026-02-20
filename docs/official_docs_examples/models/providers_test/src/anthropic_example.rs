//! Anthropic Provider Example
//! Validates: providers.md - Anthropic section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("🧠 Anthropic Provider Example");
    println!("=============================\n");
    
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-4-5-20250929"))?;

    println!("✅ Model: claude-sonnet-4-5-20250929");
    println!("📝 Key highlights:");
    println!("   • Exceptional reasoning ability");
    println!("   • Most safety-focused");
    println!("   • 200K token context\n");

    let agent = LlmAgentBuilder::new("anthropic_assistant")
        .description("Anthropic-powered assistant")
        .instruction("You are a helpful assistant powered by Anthropic Claude. Be concise and thoughtful.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
