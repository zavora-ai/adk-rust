//! OpenAI Responses API — Deep Research example.
//!
//! Demonstrates using deep research models (`o3-deep-research`, `o4-mini-deep-research`)
//! which automatically enable background mode without explicit `background: true`.
//! The example submits a research query, polls for progress, and prints the structured
//! research output with citations on completion.
//!
//! # Running
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --manifest-path examples/openai_deep_research/Cargo.toml
//! ```

use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_rust::prelude::*;
use futures::StreamExt;
use std::time::Duration;

const DEFAULT_POLL_INTERVAL_SECS: u64 = 10;
const DEFAULT_MODEL: &str = "o3-deep-research";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    println!("═══════════════════════════════════════════════════");
    println!("  OpenAI Deep Research — Automatic Background Mode");
    println!("═══════════════════════════════════════════════════");
    println!();

    let api_key = std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set — see .env.example");

    let poll_interval = std::env::var("POLL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);

    let model = std::env::var("DEEP_RESEARCH_MODEL")
        .unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    // Deep research models automatically enable background mode —
    // no explicit `background: true` needed in extensions.
    println!("🔬 Model: {model}");
    println!("⏱️  Poll interval: {poll_interval}s");
    println!();

    // Create the Responses API client with the deep research model
    let config = OpenAIResponsesConfig::new(&api_key, &model);
    let client = OpenAIResponsesClient::new(config)?;

    // Build a research query — deep research requires web_search_preview tool
    let mut gen_config = adk_rust::GenerateContentConfig::default();
    gen_config.extensions.insert(
        "openai".to_string(),
        serde_json::json!({
            "built_in_tools": [
                { "type": "web_search_preview" }
            ]
        }),
    );

    let request = LlmRequest {
        model: model.clone(),
        contents: vec![Content::new("user").with_text(
            "What are the latest advances in quantum error correction as of 2024? \
             Include key papers, research groups, and practical implications for \
             building fault-tolerant quantum computers.",
        )],
        config: Some(gen_config),
        tools: Default::default(),
        previous_response_id: None,
    };

    // Submit the research query (automatically runs in background)
    println!("📤 Submitting deep research query...");
    println!("   (Deep research models automatically enable background mode)");
    println!();

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

    // If the response already completed (unlikely for deep research), print and exit
    if initial_status == "completed" {
        println!("⚡ Research completed immediately!");
        print_research_output(&initial_response);
        println!();
        println!("✅ Example completed successfully.");
        return Ok(());
    }

    // Poll until terminal status — deep research can take minutes
    println!("🔄 Polling every {poll_interval}s for completion...");
    println!("   (Deep research typically takes 1-5 minutes)");
    println!();

    let mut poll_count = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;
        poll_count += 1;

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
                println!("✅ Deep research completed! (after {poll_count} polls)");
                println!();
                print_research_output(&poll_result);
                break;
            }
            "failed" => {
                let error_code =
                    poll_result.error_code.as_deref().unwrap_or("unknown");
                let error_message =
                    poll_result.error_message.as_deref().unwrap_or("No error message");
                println!("❌ Research failed!");
                println!("   Error code: {error_code}");
                println!("   Error message: {error_message}");
                return Err(anyhow::anyhow!("Deep research failed: {error_message}"));
            }
            "cancelled" => {
                println!("⚠️  Research was cancelled.");
                return Err(anyhow::anyhow!("Deep research was cancelled"));
            }
            other => {
                let elapsed = poll_count as u64 * poll_interval;
                println!("   ⏳ [{elapsed}s] Status: {other} — researching...");
            }
        }
    }

    println!();
    println!("✅ Example completed successfully.");
    Ok(())
}

/// Print the structured research output including citations.
fn print_research_output(response: &LlmResponse) {
    println!("═══════════════════════════════════════════════════");
    println!("  Research Output");
    println!("═══════════════════════════════════════════════════");
    println!();

    // Print the main research content
    if let Some(content) = &response.content {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("{text}");
            }
        }
    } else {
        println!("(No content in response)");
    }

    // Print citations from provider_metadata if available
    if let Some(metadata) = &response.provider_metadata {
        if let Some(openai) = metadata.get("openai") {
            if let Some(citations) = openai.get("citations") {
                if let Some(arr) = citations.as_array() {
                    if !arr.is_empty() {
                        println!();
                        println!("───────────────────────────────────────────────────");
                        println!("  Citations ({} sources)", arr.len());
                        println!("───────────────────────────────────────────────────");
                        for (i, citation) in arr.iter().enumerate() {
                            let title = citation
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Untitled");
                            let url = citation
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            println!("  [{}] {title}", i + 1);
                            if !url.is_empty() {
                                println!("      {url}");
                            }
                        }
                    }
                }
            }
        }
    }
}
