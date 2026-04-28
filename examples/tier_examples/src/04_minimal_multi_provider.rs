//! # 04 — Minimal Multi-Provider
//!
//! All three major providers (Gemini, OpenAI, Anthropic) are in the minimal tier.
//! This example auto-detects which API key is set and uses that provider.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
//! ```

use adk_rust::prelude::*;
use adk_rust::Launcher;

fn detect_model() -> anyhow::Result<(Arc<dyn Llm>, &'static str)> {
    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        let model = GeminiModel::new(&key, "gemini-2.5-flash")?;
        return Ok((Arc::new(model), "Gemini"));
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let model = OpenAIClient::new(OpenAIConfig::new(key, "gpt-5-mini"))?;
        return Ok((Arc::new(model), "OpenAI"));
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let model = AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-4-6"))?;
        return Ok((Arc::new(model), "Anthropic"));
    }
    anyhow::bail!("Set GOOGLE_API_KEY, OPENAI_API_KEY, or ANTHROPIC_API_KEY")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let (model, provider) = detect_model()?;
    println!("Using provider: {provider}\n");

    let agent = LlmAgentBuilder::new("multi-provider-agent")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
