//! Mid-stream Anthropic failures surface as errors, never as a silent success.
#![cfg(feature = "anthropic")]

use adk_core::{AdkError, Content, ErrorCategory, Llm, LlmRequest, LlmResponse, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MESSAGE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4-6\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n";

fn tool_use_start(name: &str) -> String {
    format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\n"
    )
}

fn input_json_delta(partial_json: &str) -> String {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "input_json_delta", "partial_json": partial_json}
    });
    format!("event: content_block_delta\ndata: {event}\n\n")
}

const BLOCK_STOP: &str =
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
const TOOL_USE_DELTA: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n";
const MESSAGE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

async fn stream_items(sse_body: String) -> Vec<Result<LlmResponse, AdkError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let client = AnthropicClient::new(
        AnthropicConfig::new("test-key", "claude-sonnet-4-6").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let mut request =
        LlmRequest::new("claude-sonnet-4-6", vec![Content::new("user").with_text("Hello")]);
    request.tools.insert(
        "get_weather".to_string(),
        serde_json::json!({"description": "Weather", "parameters": {"type": "object"}}),
    );

    client.generate_content(request, true).await.unwrap().collect().await
}

#[tokio::test]
async fn overloaded_error_event_fails_the_stream_with_a_retryable_error() {
    let body = format!(
        "{MESSAGE_START}event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}}}\n\n"
    );

    let items = stream_items(body).await;

    let error = items.last().expect("stream yields items").as_ref().unwrap_err();
    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert!(error.is_retryable());
    assert!(error.message.contains("Overloaded"), "unexpected message: {}", error.message);
}

#[tokio::test]
async fn rate_limit_error_event_maps_to_rate_limited() {
    let body = format!(
        "{MESSAGE_START}event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"rate_limit_error\",\"message\":\"Slow down\"}}}}\n\n"
    );

    let items = stream_items(body).await;

    let error = items.last().expect("stream yields items").as_ref().unwrap_err();
    assert_eq!(error.category, ErrorCategory::RateLimited);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn truncated_tool_arguments_fail_instead_of_becoming_empty() {
    let body = format!(
        "{MESSAGE_START}{}{}{BLOCK_STOP}{TOOL_USE_DELTA}{MESSAGE_STOP}",
        tool_use_start("get_weather"),
        input_json_delta("{\"city\": \"Nai"),
    );

    let items = stream_items(body).await;

    let error = items.last().expect("stream yields items").as_ref().unwrap_err();
    assert_eq!(error.code, "model.anthropic.invalid_tool_arguments");
    assert!(error.message.contains("'get_weather'"));
}

#[tokio::test]
async fn zero_argument_tool_call_has_empty_object_arguments() {
    let body = format!(
        "{MESSAGE_START}{}{BLOCK_STOP}{TOOL_USE_DELTA}{MESSAGE_STOP}",
        tool_use_start("get_weather")
    );

    let items = stream_items(body).await;

    let calls: Vec<Part> = items
        .into_iter()
        .map(|item| item.expect("stream succeeds"))
        .filter_map(|response| response.content)
        .flat_map(|content| content.parts)
        .collect();
    assert_eq!(
        calls,
        vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: serde_json::json!({}),
            id: Some("toolu_1".to_string()),
            thought_signature: None,
        }]
    );
}
