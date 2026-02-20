//! Anthropic Retry & Error Handling Example
//!
//! Demonstrates custom retry configuration, structured error handling,
//! and rate limit inspection. Shows how the client handles retryable
//! errors (429, 529) with configurable backoff.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_retry --features anthropic
//! ```

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::RetryConfig;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, RateLimitInfo};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;

fn make_request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest { model: String::new(), contents, config: None, tools: HashMap::new() }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    // --- Custom Retry Configuration ---
    println!("=== Retry Configuration ===\n");

    // Configure aggressive retry for high-throughput scenarios
    let retry_config = RetryConfig::default()
        .with_max_retries(5)
        .with_initial_delay(Duration::from_millis(500))
        .with_max_delay(Duration::from_secs(30))
        .with_backoff_multiplier(2.0);

    println!("  Max retries:        {}", retry_config.max_retries);
    println!("  Initial delay:      {:?}", retry_config.initial_delay);
    println!("  Max delay:          {:?}", retry_config.max_delay);
    println!("  Backoff multiplier: {}", retry_config.backoff_multiplier);

    let client = AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-4-20250514"))?
        .with_retry_config(retry_config);

    // --- Successful Request ---
    println!("\n=== Successful Request ===\n");

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "Say 'hello' and nothing else.".to_string() }],
    }]);

    let mut stream = client.generate_content(request, false).await?;
    if let Some(Ok(response)) = stream.next().await
        && let Some(content) = &response.content
    {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("  Response: {text}");
            }
        }
    }

    // --- Rate Limit Inspection ---
    println!("\n=== Rate Limit Info ===\n");
    let info: RateLimitInfo = client.latest_rate_limit_info().await;
    println!("  Requests limit:     {:?}", info.requests_limit);
    println!("  Requests remaining: {:?}", info.requests_remaining);
    println!("  Requests reset:     {:?}", info.requests_reset);
    println!("  Tokens limit:       {:?}", info.tokens_limit);
    println!("  Tokens remaining:   {:?}", info.tokens_remaining);
    println!("  Tokens reset:       {:?}", info.tokens_reset);
    println!("  Retry-after:        {:?}", info.retry_after);

    // --- Structured Error Handling ---
    println!("\n=== Structured Error Handling ===\n");

    // Intentionally use an invalid API key to trigger a structured error
    let bad_client = AnthropicClient::new(AnthropicConfig::new(
        "sk-ant-invalid-key",
        "claude-sonnet-4-20250514",
    ))?
    .with_retry_config(RetryConfig::disabled());

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "test".to_string() }],
    }]);

    match bad_client.generate_content(request, false).await {
        Ok(mut stream) => match stream.next().await {
            Some(Err(e)) => {
                println!("  Error (expected): {e}");
                // The error message contains structured info:
                // type, status code, message, and request-id when available
            }
            Some(Ok(_)) => println!("  Unexpected success with invalid key"),
            None => println!("  Empty stream"),
        },
        Err(e) => println!("  Error (expected): {e}"),
    }

    // --- Disabled Retry ---
    println!("\n=== Disabled Retry (fail fast) ===\n");

    let no_retry_client =
        AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-4-20250514"))?
            .with_retry_config(RetryConfig::disabled());

    println!("  Retry enabled: {}", no_retry_client.retry_config().enabled);
    println!("  (Errors will not be retried — useful for testing)\n");

    let request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "Quick test with no retry.".to_string() }],
    }]);

    let mut stream = no_retry_client.generate_content(request, false).await?;
    if let Some(Ok(response)) = stream.next().await
        && let Some(content) = &response.content
    {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("  Response: {text}");
            }
        }
    }

    println!("\nDone!");
    Ok(())
}
