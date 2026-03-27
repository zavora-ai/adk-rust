#![allow(clippy::result_large_err)]

#[path = "openrouter/chat_options.rs"]
mod chat_options;
#[path = "openrouter/chat_support.rs"]
mod chat_support;
#[path = "openrouter/common.rs"]
mod common;

use adk_model::openrouter::{
    OpenRouterApiMode, OpenRouterChatMessage, OpenRouterChatMessageContent, OpenRouterChatRequest,
    OpenRouterChatStreamItem, OpenRouterFunctionDescription, OpenRouterPlugin,
    OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterTool,
};
use anyhow::Result;
use futures::StreamExt;
use serde_json::json;

const MAX_COMPLETION_TOKENS: i32 = 256;

#[tokio::main]
async fn main() -> Result<()> {
    let (client, config) = common::build_client(OpenRouterApiMode::ChatCompletions)?;

    common::print_section("native chat non-streaming");
    let response = client
        .send_chat(OpenRouterChatRequest {
            model: config.model.clone(),
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "Give a short explanation of why developers use OpenRouter for routing."
                        .to_string(),
                )),
                ..Default::default()
            }],
            plugins: chat_options::web_plugin_enabled().then_some(vec![OpenRouterPlugin {
                id: "web".to_string(),
                enabled: Some(true),
                ..Default::default()
            }]),
            provider: Some(OpenRouterProviderPreferences {
                allow_fallbacks: Some(true),
                ..Default::default()
            }),
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("medium".to_string()),
                ..Default::default()
            }),
            max_completion_tokens: Some(MAX_COMPLETION_TOKENS),
            ..Default::default()
        })
        .await?;
    chat_support::print_chat_response(&response);

    common::print_section("native chat streaming");
    let mut stream = client
        .send_chat_stream(OpenRouterChatRequest {
            model: config.model.clone(),
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "Output exactly five lines containing only these values in order: 1, 2, 3, 4, 5."
                        .to_string(),
                )),
                ..Default::default()
            }],
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("low".to_string()),
                ..Default::default()
            }),
            max_completion_tokens: Some(MAX_COMPLETION_TOKENS),
            ..Default::default()
        })
        .await?;

    while let Some(item) = stream.next().await {
        match item? {
            OpenRouterChatStreamItem::Chunk(chunk) => {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(delta) = choice.delta.as_ref() {
                        if let Some(reasoning) = delta.reasoning.as_deref() {
                            println!("reasoning.delta: {}", reasoning.trim());
                        }
                        if let Some(text) = chat_support::chat_message_text(delta) {
                            print!("{text}");
                        }
                    }
                    if let Some(finish_reason) = choice.finish_reason.as_deref() {
                        println!("\nfinish_reason: {finish_reason}");
                    }
                }
            }
            OpenRouterChatStreamItem::Done => println!("\n[stream completed]"),
            OpenRouterChatStreamItem::Error(error) => println!("stream error: {}", error.message),
        }
    }

    common::print_section("native chat tool calling");
    let response = client
        .send_chat(OpenRouterChatRequest {
            model: config.model,
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "A function named get_weather is available. Call it with city=\"Nairobi\"."
                        .to_string(),
                )),
                ..Default::default()
            }],
            tools: Some(vec![OpenRouterTool {
                kind: "function".to_string(),
                function: Some(OpenRouterFunctionDescription {
                    name: "get_weather".to_string(),
                    description: Some("Look up the current weather for a city.".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            max_completion_tokens: Some(MAX_COMPLETION_TOKENS),
            ..Default::default()
        })
        .await?;
    chat_support::print_chat_response(&response);

    Ok(())
}
