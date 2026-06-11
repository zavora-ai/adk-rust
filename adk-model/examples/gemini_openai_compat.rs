//! Gemini via the OpenAI-compatible endpoint.
//!
//! Gemini models are reachable through the OpenAI Chat Completions wire format
//! at `https://generativelanguage.googleapis.com/v1beta/openai`. ADK-Rust exposes
//! this with the `OpenAICompatibleConfig::gemini(...)` preset, so a single
//! OpenAI-compatible code path works across providers using a `GEMINI_API_KEY`.
//!
//! This example demonstrates, all against a real Gemini model:
//!   1. Basic chat
//!   2. Reasoning effort (maps to Gemini thinking levels/budgets)
//!   3. Gemini-specific thinking config via the `extra_body` channel
//!   4. Streaming
//!   5. Function calling (tool calls)
//!   6. Structured JSON output (response schema)
//!
//! For native Gemini features (server-side tools, the Interactions API, native
//! `ThinkingConfig`), prefer `adk_model::gemini::GeminiModel`. This preset is for
//! callers who want one uniform OpenAI-compatible client across providers.
//!
//! ```bash
//! GEMINI_API_KEY=... cargo run -p adk-model --features openai --example gemini_openai_compat
//! ```

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use async_openai::types::chat::ReasoningEffort;
use futures::StreamExt;
use serde_json::json;

const MODEL: &str = "gemini-3.5-flash";

fn api_key() -> String {
    std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .expect("set GEMINI_API_KEY or GOOGLE_API_KEY")
}

/// Build a single-turn user request.
fn user_request(text: &str) -> LlmRequest {
    LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(text)],
        config: None,
        tools: Default::default(),
        previous_response_id: None,
    }
}

/// Print the text/thinking parts and usage of a non-streaming response.
async fn print_response(model: &OpenAICompatible, request: LlmRequest) {
    match model.generate_content(request, false).await {
        Ok(mut stream) => {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        if let Some(content) = &response.content {
                            for part in &content.parts {
                                match part {
                                    Part::Thinking { thinking, .. } if !thinking.is_empty() => {
                                        println!("    💭 {thinking}");
                                    }
                                    Part::Text { text } if !text.trim().is_empty() => {
                                        println!("    📝 {}", text.trim());
                                    }
                                    Part::FunctionCall { name, args, .. } => {
                                        println!("    🔧 tool call: {name}({args})");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if let Some(u) = &response.usage_metadata {
                            println!(
                                "    📊 {} prompt + {} output = {} total",
                                u.prompt_token_count, u.candidates_token_count, u.total_token_count
                            );
                        }
                    }
                    Err(e) => println!("    ❌ {e}"),
                }
            }
        }
        Err(e) => println!("    ❌ {e}"),
    }
    println!();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let key = api_key();

    println!("=== Gemini via OpenAI-compatible endpoint ({MODEL}) ===\n");

    // ---------------------------------------------------------------
    // 1. Basic chat — three-line change from any OpenAI integration.
    // ---------------------------------------------------------------
    println!("--- 1. Basic chat ---");
    let model = OpenAICompatible::new(OpenAICompatibleConfig::gemini(&key, MODEL))?;
    print_response(&model, user_request("Explain how AI works in two sentences.")).await;

    // ---------------------------------------------------------------
    // 2. Reasoning effort — maps to Gemini thinking levels/budgets.
    //    OpenAI `reasoning_effort` ⇄ Gemini `thinking_level`/`thinking_budget`.
    // ---------------------------------------------------------------
    println!("--- 2. Reasoning effort: low ---");
    let model = OpenAICompatible::new(
        OpenAICompatibleConfig::gemini(&key, MODEL).with_reasoning_effort(ReasoningEffort::Low),
    )?;
    print_response(&model, user_request("What is 17 * 23? Show your work briefly.")).await;

    // ---------------------------------------------------------------
    // 3. Gemini thinking config via `extra_body`.
    //    The compatibility layer reads `google.thinking_config`; we pass it
    //    through the request's `extensions["openai"]` map, which the client
    //    merges verbatim into the request body.
    // ---------------------------------------------------------------
    println!("--- 3. Thinking config via extra_body (include_thoughts) ---");
    let model = OpenAICompatible::new(OpenAICompatibleConfig::gemini(&key, MODEL))?;
    let config = GenerateContentConfig {
        extensions: {
            let mut ext = serde_json::Map::new();
            ext.insert(
                "openai".to_string(),
                json!({
                    "extra_body": {
                        "google": {
                            "thinking_config": {
                                "thinking_level": "low",
                                "include_thoughts": true
                            }
                        }
                    }
                }),
            );
            ext
        },
        ..Default::default()
    };
    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text("Why is the sky blue?")],
        config: Some(config),
        tools: Default::default(),
        previous_response_id: None,
    };
    print_response(&model, request).await;

    // ---------------------------------------------------------------
    // 4. Streaming.
    // ---------------------------------------------------------------
    println!("--- 4. Streaming ---");
    let model = OpenAICompatible::new(OpenAICompatibleConfig::gemini(&key, MODEL))?;
    match model.generate_content(user_request("Write a haiku about Rust."), true).await {
        Ok(mut stream) => {
            print!("    ");
            while let Some(result) = stream.next().await {
                if let Ok(response) = result
                    && let Some(content) = &response.content
                {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            print!("{text}");
                        }
                    }
                }
            }
            println!("\n");
        }
        Err(e) => println!("    ❌ {e}\n"),
    }

    // ---------------------------------------------------------------
    // 5. Function calling.
    // ---------------------------------------------------------------
    println!("--- 5. Function calling ---");
    let model = OpenAICompatible::new(OpenAICompatibleConfig::gemini(&key, MODEL))?;
    let mut tools = std::collections::HashMap::new();
    tools.insert(
        "get_weather".to_string(),
        json!({
            "name": "get_weather",
            "description": "Get the current weather in a given location",
            "parameters": {
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "The city and state, e.g. Chicago, IL"
                    },
                    "unit": { "type": "string", "enum": ["celsius", "fahrenheit"] }
                },
                "required": ["location"]
            }
        }),
    );
    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text("What's the weather like in Chicago today?")],
        config: None,
        tools,
        previous_response_id: None,
    };
    print_response(&model, request).await;

    // ---------------------------------------------------------------
    // 6. Structured JSON output via a response schema.
    // ---------------------------------------------------------------
    println!("--- 6. Structured output (JSON schema) ---");
    let model = OpenAICompatible::new(OpenAICompatibleConfig::gemini(&key, MODEL))?;
    let config = GenerateContentConfig {
        response_schema: Some(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "date": { "type": "string" },
                "participants": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["name", "date", "participants"]
        })),
        ..Default::default()
    };
    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(
            "Extract the event: John and Susan are going to an AI conference on Friday.",
        )],
        config: Some(config),
        tools: Default::default(),
        previous_response_id: None,
    };
    print_response(&model, request).await;

    println!("=== Done ===");
    Ok(())
}
