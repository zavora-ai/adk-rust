#![allow(clippy::result_large_err)]

#[path = "openrouter/common.rs"]
mod common;
#[path = "openrouter/responses_options.rs"]
mod responses_options;
#[path = "openrouter/responses_support.rs"]
mod responses_support;

use adk_model::openrouter::{
    OpenRouterApiMode, OpenRouterReasoningConfig, OpenRouterResponseInput,
    OpenRouterResponseInputContent, OpenRouterResponseInputItem, OpenRouterResponseTool,
    OpenRouterResponsesRequest, OpenRouterResponsesStreamItem,
};
use anyhow::Result;
use futures::StreamExt;
use serde_json::json;

const MAX_OUTPUT_TOKENS: i32 = 256;

#[tokio::main]
async fn main() -> Result<()> {
    let (client, config) = common::build_client(OpenRouterApiMode::Responses)?;

    common::print_section("responses non-streaming");
    let response = client
        .create_response(OpenRouterResponsesRequest {
            model: Some(config.model.clone()),
            input: Some(OpenRouterResponseInput::Items(vec![OpenRouterResponseInputItem {
                kind: "message".to_string(),
                role: Some("user".to_string()),
                content: Some(OpenRouterResponseInputContent::Text(
                    "Explain the difference between chat completions and responses APIs in two short sentences."
                        .to_string(),
                )),
                ..Default::default()
            }])),
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("medium".to_string()),
                ..Default::default()
            }),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await?;
    responses_support::print_responses_response(&response);

    if let Some(previous_response_id) = response.id.as_ref() {
        common::print_section("responses chaining");
        match client
            .create_response(OpenRouterResponsesRequest {
                model: Some(config.model.clone()),
                previous_response_id: Some(previous_response_id.clone()),
                input: Some(OpenRouterResponseInput::Text(
                    "Rewrite your immediately previous answer in exactly five words.".to_string(),
                )),
                max_output_tokens: Some(MAX_OUTPUT_TOKENS),
                ..Default::default()
            })
            .await
        {
            Ok(chained) => responses_support::print_responses_response(&chained),
            Err(err) => println!("chaining request failed for this model/account setup: {err}"),
        }
    } else {
        println!("skipping chaining because the response id was not returned");
    }

    common::print_section("responses streaming");
    let mut stream = client
        .create_response_stream(OpenRouterResponsesRequest {
            model: Some(config.model.clone()),
            input: Some(OpenRouterResponseInput::Text(
                "List three benefits of provider routing.".to_string(),
            )),
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("low".to_string()),
                ..Default::default()
            }),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await?;

    while let Some(item) = stream.next().await {
        match item? {
            OpenRouterResponsesStreamItem::Event(event) => match event.kind.as_str() {
                "response.output_text.delta" => {
                    if let Some(delta) = event.delta.as_deref() {
                        print!("{delta}");
                    }
                }
                "response.reasoning.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = event.delta.as_deref().or(event.text.as_deref()) {
                        println!("\nreasoning.delta: {}", delta.trim());
                    }
                }
                "response.completed" => println!("\n[stream completed]"),
                _ => {}
            },
            OpenRouterResponsesStreamItem::Done => println!("[DONE]"),
            OpenRouterResponsesStreamItem::Error(error) => {
                println!("stream error: {}", error.message);
            }
        }
    }

    common::print_section("responses tool calling");
    let response = client
        .create_response(OpenRouterResponsesRequest {
            model: Some(config.model.clone()),
            input: Some(OpenRouterResponseInput::Text(
                "A function named get_weather is available. Call it with city=\"Nairobi\"."
                    .to_string(),
            )),
            tools: Some(vec![OpenRouterResponseTool {
                kind: "function".to_string(),
                name: Some("get_weather".to_string()),
                description: Some("Look up the current weather for a city.".to_string()),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    },
                    "required": ["city"]
                })),
                ..Default::default()
            }]),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await?;
    responses_support::print_responses_response(&response);

    if responses_options::web_search_enabled() {
        common::print_section("responses built-in web search");
        let response = client
            .create_response(OpenRouterResponsesRequest {
                model: Some(config.model),
                input: Some(OpenRouterResponseInput::Text(
                    "Find one recent fact about Rust programming language releases and cite the source."
                        .to_string(),
                )),
                tools: Some(vec![OpenRouterResponseTool {
                    kind: responses_options::web_search_tool(),
                    ..Default::default()
                }]),
                max_output_tokens: Some(MAX_OUTPUT_TOKENS),
                ..Default::default()
            })
            .await?;
        responses_support::print_responses_response(&response);
    } else {
        println!("skipping built-in web search; set OPENROUTER_ENABLE_WEB_SEARCH=1 to run it");
    }

    Ok(())
}
