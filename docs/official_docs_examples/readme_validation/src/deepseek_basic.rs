//! README DeepSeek Example - Validates DeepSeekClient::chat pattern

use adk_rust::prelude::*;
use adk_rust::Launcher;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;

    // Standard chat model
    let model = DeepSeekClient::chat(api_key)?;

    // Or use reasoner for chain-of-thought reasoning
    // let model = DeepSeekClient::reasoner(api_key)?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
