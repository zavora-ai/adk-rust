//! Gemini Token Usage Metadata Example
//!
//! Demonstrates how to inspect `UsageMetadata` returned by the Gemini API,
//! including prompt tokens, candidate tokens, thinking tokens, and cache
//! token counts. Uses Gemini 2.5 Flash which supports thinking natively.
//!
//! The response metadata includes:
//! - `prompt_token_count` — tokens consumed by the input
//! - `candidates_token_count` — tokens generated in the output
//! - `total_token_count` — sum of prompt + candidates
//! - `thinking_token_count` — tokens used for internal reasoning (Gemini 2.5)
//! - `cache_read_input_token_count` — tokens served from cache (if caching active)
//! - `cache_creation_input_token_count` — tokens used to populate cache
//!
//! ```bash
//! export GOOGLE_API_KEY=...
//! cargo run --example gemini_token_usage
//! ```

use adk_core::{Content, Llm, LlmRequest, Part, UsageMetadata};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;
use std::collections::HashMap;

/// Pretty-print all fields of UsageMetadata.
fn print_usage(label: &str, usage: &UsageMetadata) {
    println!("--- {label} ---");
    println!("  prompt tokens:           {}", usage.prompt_token_count);
    println!("  candidate tokens:        {}", usage.candidates_token_count);
    println!("  total tokens:            {}", usage.total_token_count);
    if let Some(thinking) = usage.thinking_token_count {
        println!("  thinking tokens:         {thinking}");
    }
    if let Some(cache_read) = usage.cache_read_input_token_count {
        println!("  cache read tokens:       {cache_read}");
    }
    if let Some(cache_create) = usage.cache_creation_input_token_count {
        println!("  cache creation tokens:   {cache_create}");
    }
    if let Some(audio_in) = usage.audio_input_token_count {
        println!("  audio input tokens:      {audio_in}");
    }
    if let Some(audio_out) = usage.audio_output_token_count {
        println!("  audio output tokens:     {audio_out}");
    }
    println!();
}

/// Send a request and collect the final response with usage metadata.
async fn send_request(
    model: &GeminiModel,
    prompt: &str,
) -> Result<(String, Option<UsageMetadata>), Box<dyn std::error::Error>> {
    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(prompt)],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = model.generate_content(request, true).await?;
    let mut text = String::new();
    let mut final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                match part {
                    Part::Thinking { thinking, .. } => {
                        if text.is_empty() {
                            println!("  [thinking] {}", &thinking[..thinking.len().min(80)]);
                        }
                    }
                    Part::Text { text: t } => text.push_str(t),
                    _ => {}
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    Ok((text, final_usage))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Gemini 2.5 Flash supports thinking natively
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    println!("=== Gemini Token Usage Metadata Demo ===\n");

    // 1. Simple question — minimal thinking
    println!(">> Request 1: Simple factual question\n");
    let (answer, usage) = send_request(&model, "What is the capital of France?").await?;
    println!("  Answer: {answer}");
    if let Some(u) = &usage {
        print_usage("Request 1 Usage", u);
    }

    // 2. Reasoning question — triggers more thinking tokens
    println!(">> Request 2: Math reasoning (triggers thinking tokens)\n");
    let (answer, usage) = send_request(
        &model,
        "If a train travels 120 km in 1.5 hours, and then 80 km in 45 minutes, \
         what is the average speed for the entire journey in km/h?",
    )
    .await?;
    println!("  Answer: {answer}");
    if let Some(u) = &usage {
        print_usage("Request 2 Usage", u);
    }

    // 3. Long input — shows prompt token scaling
    println!(">> Request 3: Long input (high prompt token count)\n");
    let long_text = "The quick brown fox jumps over the lazy dog. ".repeat(100);
    let prompt = format!("Summarize the following text in one sentence:\n\n{long_text}");
    let (answer, usage) = send_request(&model, &prompt).await?;
    println!("  Answer: {answer}");
    if let Some(u) = &usage {
        print_usage("Request 3 Usage", u);
    }

    println!("=== Summary ===");
    println!("• thinking_token_count reflects internal reasoning effort");
    println!("• Harder problems consume more thinking tokens");
    println!("• prompt_token_count scales with input length");
    println!("• cache_read/creation tokens appear when prompt caching is active");

    Ok(())
}
