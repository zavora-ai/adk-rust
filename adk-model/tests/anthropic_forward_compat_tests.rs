//! A block, delta, event, or stop reason newer than this client never fails the turn.
#![cfg(feature = "anthropic")]

use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FUTURE_BLOCK: &str = r#"{"type":"future_block","payload":{"nested":[1,2]}}"#;

fn message(content: Value, stop_reason: Value) -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-4-6",
        "content": content, "stop_reason": stop_reason, "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 5}
    })
}

fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// A text turn with `extra` events after the text delta and `trailing` events
/// after the text block, ending with `stop_reason`.
fn text_stream(extra: &[String], trailing: &[String], stop_reason: &str) -> String {
    let mut body = event(
        "message_start",
        json!({"type": "message_start", "message": message(json!([]), Value::Null)}),
    );
    body += &event(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    );
    body += &event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hello"}}),
    );
    body += &extra.concat();
    body += &event("content_block_stop", json!({"type": "content_block_stop", "index": 0}));
    body += &trailing.concat();
    body += &event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop_reason, "stop_sequence": null}, "usage": {"output_tokens": 5}}),
    );
    body + &event("message_stop", json!({"type": "message_stop"}))
}

async fn generate(response: ResponseTemplate, stream: bool) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(response)
        .mount(&server)
        .await;
    let client = AnthropicClient::new(
        AnthropicConfig::new("test-key", "claude-sonnet-4-6").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let request = LlmRequest::new("claude-sonnet-4-6", vec![Content::new("user").with_text("Hi")]);
    let items: Vec<_> = client.generate_content(request, stream).await.unwrap().collect().await;
    items.into_iter().map(|item| item.expect("the turn succeeds")).collect()
}

async fn generate_stream(body: String) -> Vec<LlmResponse> {
    let response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body);
    generate(response, true).await
}

fn final_text(responses: &[LlmResponse]) -> String {
    let last = responses.last().expect("the turn yields a response");
    last.content
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The provider-native copy of the turn that the next request replays.
fn native_blocks(responses: &[LlmResponse]) -> Vec<Value> {
    let last = responses.last().expect("the turn yields a response");
    last.content
        .iter()
        .flat_map(|content| &content.parts)
        .find_map(|part| match part {
            Part::ServerToolResponse { server_tool_response } => {
                server_tool_response["content"].as_array().cloned()
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn streamed_unknown_block_is_skipped_and_kept_for_replay() {
    let block = format!(
        "{}{}",
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": serde_json::from_str::<Value>(FUTURE_BLOCK).unwrap()})
        ),
        event("content_block_stop", json!({"type": "content_block_stop", "index": 1}))
    );

    let responses = generate_stream(text_stream(&[], &[block], "end_turn")).await;

    assert_eq!(final_text(&responses), "hello");
    assert_eq!(responses.last().unwrap().finish_reason, Some(FinishReason::Stop));
    assert_eq!(
        native_blocks(&responses),
        vec![
            json!({"type": "text", "text": "hello"}),
            serde_json::from_str::<Value>(FUTURE_BLOCK).unwrap()
        ]
    );
}

#[tokio::test]
async fn streamed_unknown_delta_is_skipped() {
    let delta = event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "future_delta", "payload": "x"}}),
    );

    let responses = generate_stream(text_stream(&[delta], &[], "end_turn")).await;

    assert_eq!(final_text(&responses), "hello");
}

#[tokio::test]
async fn streamed_unknown_event_is_skipped() {
    let future =
        event("future_event", json!({"type": "future_event", "payload": {"important": "data"}}));

    let responses = generate_stream(text_stream(&[future], &[], "end_turn")).await;

    assert_eq!(final_text(&responses), "hello");
}

#[tokio::test]
async fn streamed_unknown_stop_reason_finishes_as_other() {
    let responses = generate_stream(text_stream(&[], &[], "future_reason")).await;

    assert_eq!(final_text(&responses), "hello");
    assert_eq!(responses.last().unwrap().finish_reason, Some(FinishReason::Other));
}

#[tokio::test]
async fn unary_unknown_block_and_stop_reason_do_not_fail_the_turn() {
    let body = message(
        json!([{"type": "text", "text": "hello"}, serde_json::from_str::<Value>(FUTURE_BLOCK).unwrap()]),
        json!("future_reason"),
    );

    let responses = generate(ResponseTemplate::new(200).set_body_json(body), false).await;

    assert_eq!(final_text(&responses), "hello");
    assert_eq!(responses.last().unwrap().finish_reason, Some(FinishReason::Other));
    assert_eq!(
        native_blocks(&responses),
        vec![
            json!({"type": "text", "text": "hello"}),
            serde_json::from_str::<Value>(FUTURE_BLOCK).unwrap()
        ]
    );
}
