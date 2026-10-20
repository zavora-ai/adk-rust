//! Mid-stream Anthropic failures surface as errors, never as a silent success, and a
//! malformed or cut-off tool call is marked for the agent to answer, never run with
//! substituted input.
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
const MAX_TOKENS_DELTA: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n";
const TOOL_USE_DELTA: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n";
const MESSAGE_STOP: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

fn text_message() -> String {
    format!(
        "{MESSAGE_START}event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\nevent: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"Answer\"}}}}\n\n{BLOCK_STOP}event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":5}}}}\n\n"
    )
}

async fn stream_items(sse_body: String) -> Vec<Result<LlmResponse, AdkError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .expect(1)
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
async fn incomplete_streams_remain_retryable() {
    for body in [MESSAGE_START.to_owned(), text_message()] {
        let items = stream_items(body).await;
        let error =
            items.last().expect("unfinished stream reports a failure").as_ref().unwrap_err();
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.code, "model.anthropic.unfinished_stream");
        assert!(error.is_retryable());
        assert!(
            items.iter().filter_map(|item| item.as_ref().ok()).all(|response| response.partial)
        );
    }
}

#[tokio::test]
async fn completed_messages_keep_their_answer() {
    let items = stream_items(format!("{}{MESSAGE_STOP}", text_message())).await;
    let replies = items.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    let complete: Vec<_> = replies.iter().filter(|response| !response.partial).collect();
    assert_eq!(complete.len(), 1);
    let response = complete[0];
    assert!(response.turn_complete);
    assert_eq!(
        response.content.as_ref().unwrap().parts,
        vec![Part::Text { text: "Answer".into() }]
    );
    assert_eq!(response.usage_metadata.as_ref().unwrap().candidates_token_count, 5);
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

/// The one function call the stream completed with.
fn only_call(items: Vec<Result<LlmResponse, AdkError>>) -> (String, serde_json::Value) {
    let calls: Vec<Part> = items
        .into_iter()
        .map(|item| item.expect("stream succeeds"))
        .filter(|response| !response.partial)
        .filter_map(|response| response.content)
        .flat_map(|content| content.parts)
        .filter(|part| matches!(part, Part::FunctionCall { .. }))
        .collect();
    match calls.as_slice() {
        [Part::FunctionCall { name, args, .. }] => (name.clone(), args.clone()),
        other => panic!("expected one function call, got {other:?}"),
    }
}

#[tokio::test]
async fn truncated_tool_arguments_are_marked_instead_of_becoming_empty() {
    let body = format!(
        "{MESSAGE_START}{}{}{BLOCK_STOP}{TOOL_USE_DELTA}{MESSAGE_STOP}",
        tool_use_start("get_weather"),
        input_json_delta("{\"city\": \"Nai"),
    );

    let (name, args) = only_call(stream_items(body).await);
    assert_eq!(name, "get_weather");
    let reason = adk_core::invalid_tool_arguments_reason(&args).expect("marked arguments");
    assert!(reason.contains("not valid JSON"), "{reason}");
}

#[tokio::test]
async fn tool_call_cut_off_by_max_tokens_is_marked_instead_of_being_dropped() {
    let body = format!(
        "{MESSAGE_START}{}{}{BLOCK_STOP}{MAX_TOKENS_DELTA}{MESSAGE_STOP}",
        tool_use_start("get_weather"),
        input_json_delta("{\"city\": \"Nai"),
    );

    let (name, args) = only_call(stream_items(body).await);
    assert_eq!(name, "get_weather");
    assert!(adk_core::invalid_tool_arguments_reason(&args).is_some(), "{args}");
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
