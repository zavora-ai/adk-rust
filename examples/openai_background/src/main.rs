//! OpenAI Responses API — Background Mode example.
//!
//! Demonstrates submitting a request with `background: true` and polling for
//! completion using `poll_response()`. Background mode is useful for long-running
//! requests where you don't want to hold open a connection.
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_background/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use futures::StreamExt;
use std::time::Duration;

const DEFAULT_POLL_INTERVAL_SECS: u64 = 2;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI Background Mode — Submit & Poll Example");
    println!("═══════════════════════════════════════════════════");
    println!();

    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set — see .env.example");

    let poll_interval = std::env::var("POLL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);

    // Create the Responses API client
    let config = OpenAIResponsesConfig::new(&api_key, "gpt-4.1-nano");
    let client = OpenAIResponsesClient::new(config)?;

    // Build a request with background: true via extensions
    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "background": true
        }),
    );

    let request = LlmRequest {
        model: "gpt-4.1-nano".to_string(),
        contents: vec![Content::new("user").with_text(
            "Write a short paragraph about the benefits of asynchronous programming.",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    // Submit the background request
    println!("📤 Submitting background request...");
    let mut stream = client.generate_content(request, false).await?;
    let initial_response = stream
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("No initial response received"))??;

    // Extract response_id from provider_metadata
    let response_id = initial_response
        .provider_metadata
        .as_ref()
        .and_then(|m| m.get("openai"))
        .and_then(|o| o.get("response_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No response_id in provider_metadata"))?
        .to_string();

    let initial_status = initial_response
        .provider_metadata
        .as_ref()
        .and_then(|m| m.get("openai"))
        .and_then(|o| o.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    println!("📋 Response ID: {response_id}");
    println!("📊 Initial status: {initial_status}");
    println!();

    // If the response already completed (fast model), print and exit
    if initial_status == "completed" {
        println!("⚡ Response completed immediately!");
        if let Some(content) = &initial_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("{text}");
                }
            }
        }
        println!();
        println!("✅ Example completed successfully.");
        return Ok(());
    }

    // Poll until terminal status
    println!("🔄 Polling every {poll_interval}s for completion...");
    loop {
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;

        let poll_result = client.poll_response(&response_id).await?;

        let status = poll_result
            .provider_metadata
            .as_ref()
            .and_then(|m| m.get("openai"))
            .and_then(|o| o.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match status {
            "completed" => {
                println!("✅ Response completed!");
                println!();
                if let Some(content) = &poll_result.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            println!("{text}");
                        }
                    }
                }
                break;
            }
            "failed" => {
                let error_code =
                    poll_result.error_code.as_deref().unwrap_or("unknown");
                let error_message =
                    poll_result.error_message.as_deref().unwrap_or("No error message");
                println!("❌ Response failed!");
                println!("   Error code: {error_code}");
                println!("   Error message: {error_message}");
                return Err(anyhow::anyhow!("Background response failed: {error_message}"));
            }
            "cancelled" => {
                println!("⚠️  Response was cancelled.");
                return Err(anyhow::anyhow!("Background response was cancelled"));
            }
            other => {
                println!("   ⏳ Status: {other} — still processing...");
            }
        }
    }

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}
