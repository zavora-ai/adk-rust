//! Live replay checks against the Anthropic API: a thinking tool-use turn and a web
//! fetch turn are sent back in history and accepted. Require `ANTHROPIC_API_KEY`.
#![cfg(feature = "anthropic")]

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, ThinkingMode};
use futures::TryStreamExt;
use serde_json::json;

fn client() -> Option<AnthropicClient> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    Some(AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5-5")).unwrap())
}

/// Budget thinking thinks on every turn, and a tool-use turn replayed without its
/// thinking block is rejected, so this model shows whether replay is exact.
fn always_thinking_client() -> Option<AnthropicClient> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let config = AnthropicConfig::new(api_key, "claude-haiku-4-5")
        .with_thinking_mode(ThinkingMode::Enabled { budget_tokens: 2_048 });
    Some(AnthropicClient::new(config).unwrap())
}

async fn complete(client: &AnthropicClient, request: LlmRequest, stream: bool) -> Content {
    let responses: Vec<LlmResponse> = client
        .generate_content(request, stream)
        .await
        .expect("request accepted")
        .try_collect()
        .await
        .expect("response parsed");
    // The last response carries the complete turn in both modes.
    responses.into_iter().rev().find_map(|response| response.content).expect("turn content")
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_thinking_tool_turn_replays() {
    let (Some(budget), Some(adaptive)) = (always_thinking_client(), client()) else { return };
    for (client, stream) in [(&budget, false), (&budget, true), (&adaptive, false)] {
        let thinks_every_turn = std::ptr::eq(client, &budget);
        let mut request = LlmRequest::new(
            "ignored",
            vec![
                Content::new("user")
                    .with_text("Use get_weather for Paris, then tell me whether I need a coat."),
            ],
        );
        request.tools.insert(
            "get_weather".to_string(),
            json!({
                "description": "Get the current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }),
        );

        let turn = complete(client, request.clone(), stream).await;
        let signed_thinking =
            turn.parts.iter().any(|part| matches!(part, Part::Thinking { signature: Some(_), .. }));
        assert!(
            signed_thinking || !thinks_every_turn,
            "stream={stream}: thinking blocks must be kept for replay: {turn:?}"
        );
        let call_id = turn
            .parts
            .iter()
            .find_map(|part| match part {
                Part::FunctionCall { id, .. } => id.clone(),
                _ => None,
            })
            .expect("tool call");

        request.contents.push(turn);
        request.contents.push(Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "get_weather",
                    json!({"temperature_c": 4, "conditions": "rain"}),
                ),
                id: Some(call_id),
                annotations: None,
            }],
        });
        let answer = complete(client, request, stream).await;
        assert!(
            answer.parts.iter().any(|part| matches!(part, Part::Text { text } if !text.is_empty())),
            "stream={stream}: {answer:?}"
        );
    }
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_web_fetch_turn_replays() {
    let Some(client) = client() else { return };
    let mut config = GenerateContentConfig::default();
    config.extensions.insert(
        "anthropic".to_string(),
        json!({"built_in_tools": [{"type": "web_fetch_20250910", "name": "web_fetch", "max_uses": 2}]}),
    );
    let mut request = LlmRequest::new(
        "claude-sonnet-5-5",
        vec![Content::new("user").with_text(
            "Fetch https://example.com and https://example.invalid/missing, then give each page's title or error.",
        )],
    );
    request.config = Some(config);

    let turn = complete(&client, request.clone(), false).await;
    let fetched = turn.parts.iter().any(|part| {
        matches!(part, Part::ServerToolResponse { server_tool_response }
            if server_tool_response["type"] == "web_fetch_tool_result")
    });
    assert!(fetched, "{turn:?}");

    request.contents.push(turn);
    request.contents.push(Content::new("user").with_text("Thanks. Which URL failed?"));
    let answer = complete(&client, request, false).await;
    assert!(answer.parts.iter().any(|part| matches!(part, Part::Text { .. })), "{answer:?}");
}
