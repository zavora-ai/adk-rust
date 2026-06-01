//! OpenAI Responses API — Built-In Tools example.
//!
//! Demonstrates configuring OpenAI-hosted built-in tools via request extensions.
//! Built-in tools run server-side on OpenAI's infrastructure — no external
//! infrastructure or API keys beyond `OPENAI_API_KEY` are needed.
//!
//! Scenarios:
//! 1. Image Generation — configure `image_generation` with size/quality params
//! 2. Web Search — configure `web_search` to let the model search the internet
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_builtin_tools/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI Built-In Tools — Image Gen & Web Search");
    println!("═══════════════════════════════════════════════════");
    println!();

    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set — see .env.example");

    // Create the Responses API client
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-mini");
    let client = OpenAIResponsesClient::new(config)?;

    // ─── Scenario 1: Image Generation ───────────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Scenario 1: Image Generation (built-in tool)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "built_in_tools": [
                {
                    "type": "image_generation",
                    "size": "1024x1024",
                    "quality": "low"
                }
            ]
        }),
    );

    let request = LlmRequest {
        model: "gpt-4.1-mini".to_string(),
        contents: vec![Content::new("user").with_text(
            "Generate an image of a serene mountain lake at sunset with purple and orange skies.",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    println!("📤 Sending image generation request...");
    let mut stream = client.generate_content(request, false).await?;

    while let Some(response) = stream.next().await {
        let response = response?;

        // Print any text content from the model
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("📝 Model response: {text}");
                }
            }
        }

        // Check provider_metadata for image generation results
        if let Some(metadata) = &response.provider_metadata {
            if let Some(openai_meta) = metadata.get("openai") {
                if let Some(output) = openai_meta.get("output") {
                    println!("🖼️  Image generation output detected in provider_metadata:");
                    println!(
                        "   {}",
                        serde_json::to_string_pretty(output)
                            .unwrap_or_else(|_| format!("{output:?}"))
                    );
                }
                if let Some(response_id) = openai_meta.get("response_id") {
                    println!("📋 Response ID: {response_id}");
                }
            }
        }
    }

    println!();

    // ─── Scenario 2: Web Search ─────────────────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Scenario 2: Web Search (built-in tool)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "built_in_tools": [
                { "type": "web_search" }
            ]
        }),
    );

    let request = LlmRequest {
        model: "gpt-4.1-mini".to_string(),
        contents: vec![Content::new("user").with_text(
            "What is the current Rust programming language stable version number?",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    println!("📤 Sending web search request...");
    println!("   The model will search the web to answer the question.");
    println!();

    let mut stream = client.generate_content(request, false).await?;

    while let Some(response) = stream.next().await {
        let response = response?;

        // Print any text content
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("📝 Model response: {text}");
                }
            }
        }

        // Check provider_metadata for web search results
        if let Some(metadata) = &response.provider_metadata {
            if let Some(openai_meta) = metadata.get("openai") {
                if let Some(output) = openai_meta.get("output") {
                    if let Some(arr) = output.as_array() {
                        for item in arr {
                            if item.get("type").and_then(|t| t.as_str())
                                == Some("web_search_call")
                            {
                                println!("🔍 Web search was invoked by the model");
                            }
                        }
                    }
                }
                if let Some(response_id) = openai_meta.get("response_id") {
                    println!("📋 Response ID: {response_id}");
                }
            }
        }
    }

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}
