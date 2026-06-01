//! OpenAI Responses API — Open Responses Mode example.
//!
//! Demonstrates provider-agnostic usage with a configurable endpoint. Open
//! Responses mode relaxes strict OpenAI field validation so you can connect to
//! any compatible third-party provider (LM Studio, Ollama, vLLM) without code
//! changes.
//!
//! # Running
//!
//! ```bash
//! # Using OpenAI directly (default):
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_open_responses/Cargo.toml
//!
//! # Using a local provider (e.g., LM Studio):
//! export OPEN_RESPONSES_BASE_URL=http://localhost:1234/v1
//! export OPEN_RESPONSES_MODEL=local-model
//! cargo run --manifest-path examples/openai_open_responses/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI Open Responses — Provider-Agnostic Example");
    println!("═══════════════════════════════════════════════════");
    println!();

    // Read configuration from environment with sensible defaults.
    // OPENAI_API_KEY uses unwrap_or_default() because third-party providers
    // may not require an API key at all.
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();

    let base_url = std::env::var("OPEN_RESPONSES_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

    let model = std::env::var("OPEN_RESPONSES_MODEL")
        .unwrap_or_else(|_| "gpt-4.1-nano".to_string());

    println!("🔧 Configuration:");
    println!("   Base URL: {base_url}");
    println!("   Model:    {model}");
    println!(
        "   API Key:  {}",
        if api_key.is_empty() {
            "(not set — using third-party provider)"
        } else {
            "(set)"
        }
    );
    println!();

    // Configure the client with Open Responses mode enabled and custom base URL.
    let config = OpenAIResponsesConfig::new(&api_key, &model)
        .with_open_responses_mode(true)
        .with_base_url(&base_url);

    let client = OpenAIResponsesClient::new(config)?;

    // Build a simple request
    let request = LlmRequest {
        model: model.clone(),
        contents: vec![Content::new("user").with_text(
            "Explain in one sentence what Open Responses compatibility means for AI providers.",
        )],
        config: None,
        tools: Default::default(),
        previous_response_id: None,
    };

    // Send prompt and stream the response
    println!("📤 Sending prompt to {base_url}...");
    println!();

    let mut stream = client.generate_content(request, true).await?;
    let mut received_content = false;

    while let Some(response) = stream.next().await {
        match response {
            Ok(llm_response) => {
                if let Some(content) = &llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            if !text.is_empty() {
                                print!("{text}");
                                received_content = true;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // Gracefully handle errors that may occur with third-party
                // providers due to missing OpenAI-specific fields.
                eprintln!("⚠️  Stream error (may be expected with some providers): {e}");
            }
        }
    }

    if received_content {
        println!();
    }

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}
