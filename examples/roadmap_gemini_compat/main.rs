//! Roadmap example: backward-compatible GeminiModel + additive retry config.
//!
//! Run with:
//!   cargo run --example roadmap_gemini_compat
//!
//! Required env:
//!   GOOGLE_API_KEY (or GEMINI_API_KEY)

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::{GeminiModel, RetryConfig};
use anyhow::Result;
use futures::StreamExt;
use std::{env, time::Duration};

fn google_api_key() -> Option<String> {
    env::var("GOOGLE_API_KEY").ok().or_else(|| env::var("GEMINI_API_KEY").ok())
}

async fn call_once(model: &GeminiModel, prompt: &str) -> Result<String> {
    let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text(prompt)]);
    let mut stream = model.generate_content(request, false).await?;
    let mut output = String::new();

    while let Some(chunk) = stream.next().await {
        let response = chunk?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    output.push_str(&text);
                }
            }
        }
    }

    Ok(output)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let Some(api_key) = google_api_key() else {
        println!("Set GOOGLE_API_KEY (or GEMINI_API_KEY) then re-run:");
        println!("  cargo run --example roadmap_gemini_compat");
        return Ok(());
    };

    let model_name = env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
    let prompt = env::var("ROADMAP_PROMPT").unwrap_or_else(|_| {
        "Give two concrete benefits of keeping GeminiModel::new backward compatible.".to_string()
    });

    // Existing sync constructor remains unchanged, while retry is additive.
    let model = GeminiModel::new(&api_key, &model_name)?.with_retry_config(
        RetryConfig::default()
            .with_max_retries(4)
            .with_initial_delay(Duration::from_millis(200))
            .with_max_delay(Duration::from_secs(3)),
    );

    println!("Model: {}", model.name());
    println!(
        "Retry: enabled={}, max_retries={}, initial_delay_ms={}, max_delay_ms={}",
        model.retry_config().enabled,
        model.retry_config().max_retries,
        model.retry_config().initial_delay.as_millis(),
        model.retry_config().max_delay.as_millis(),
    );

    let response = call_once(&model, &prompt).await?;
    println!("\nResponse:\n{}\n", response);
    Ok(())
}
