//! Gemini Provider Example
//! Validates: providers.md - Gemini section

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    println!("🌟 Gemini Provider Example");
    println!("==========================\n");
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash")?;

    println!("✅ Model: gemini-2.0-flash");
    println!("📝 Key highlights:");
    println!("   • Native multimodal (images, video, audio, PDF)");
    println!("   • Up to 2M token context window");
    println!("   • Fast inference\n");

    let agent = LlmAgentBuilder::new("gemini_assistant")
        .description("Gemini-powered assistant")
        .instruction("You are a helpful assistant powered by Google Gemini. Be concise.")
        .model(Arc::new(model))
        .build()?;

    println!("🚀 Starting interactive session (type 'exit' to quit)\n");
    Launcher::new(Arc::new(agent)).run().await?;
    
    Ok(())
}
