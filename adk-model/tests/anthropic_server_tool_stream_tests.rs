//! A provider-executed search streams as it happens, before the answer it informs
//! (spec 3.1 §7.33 fix 2b, §10 MD1).
#![cfg(feature = "anthropic")]

use adk_core::{Content, Llm, LlmRequest, LlmResponse, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// A searched turn as the Messages API streams it: a text block, a `server_tool_use`
/// block whose input arrives as JSON deltas, its `web_search_tool_result`, and the answer.
fn searched_turn() -> String {
    let start = |index: usize, block: Value| {
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        )
    };
    let delta = |index: usize, delta: Value| {
        event(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": delta}),
        )
    };
    let stop = |index: usize| {
        event("content_block_stop", json!({"type": "content_block_stop", "index": index}))
    };
    let mut body = event(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 1}
        }}),
    );
    body += &start(0, json!({"type": "text", "text": ""}));
    body += &delta(0, json!({"type": "text_delta", "text": "Let me check. "}));
    body += &stop(0);
    body += &start(
        1,
        json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}),
    );
    body += &delta(1, json!({"type": "input_json_delta", "partial_json": "{\"query\": "}));
    body += &delta(1, json!({"type": "input_json_delta", "partial_json": "\"rust 1.95\"}"}));
    body += &stop(1);
    body += &start(
        2,
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [{
            "type": "web_search_result", "url": "https://blog.rust-lang.org/",
            "title": "Rust Blog", "encrypted_content": "abc", "page_age": null
        }]}),
    );
    body += &stop(2);
    body += &start(3, json!({"type": "text", "text": ""}));
    body += &delta(3, json!({"type": "text_delta", "text": "Rust 1.95 is out."}));
    body += &stop(3);
    body += &event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
               "usage": {"output_tokens": 9, "server_tool_use": {"web_search_requests": 1}}}),
    );
    body + &event("message_stop", json!({"type": "message_stop"}))
}

async fn stream(body: String) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let client = AnthropicClient::new(
        AnthropicConfig::new("test-key", "claude-sonnet-5-5").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let request = LlmRequest::new("claude-sonnet-5-5", vec![Content::new("user").with_text("Hi")]);
    let items: Vec<_> = client.generate_content(request, true).await.unwrap().collect().await;
    items.into_iter().map(|item| item.expect("the turn succeeds")).collect()
}

/// What each partial response carries, in stream order.
fn partials(responses: &[LlmResponse]) -> Vec<String> {
    responses
        .iter()
        .filter(|response| response.partial)
        .flat_map(|response| response.content.iter().flat_map(|content| &content.parts))
        .map(|part| match part {
            Part::Text { text } => format!("text {text}"),
            Part::ServerToolCall { server_tool_call } => {
                format!("call {} {}", server_tool_call["id"], server_tool_call["input"])
            }
            Part::ServerToolResponse { server_tool_response } => {
                format!("result {}", server_tool_response["tool_use_id"])
            }
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn a_search_streams_at_its_block_stop_before_the_answer() {
    let responses = stream(searched_turn()).await;

    assert_eq!(
        partials(&responses),
        [
            "text Let me check. ",
            r#"call "srvtoolu_1" {"query":"rust 1.95"}"#,
            r#"result "srvtoolu_1""#,
            "text Rust 1.95 is out.",
        ]
    );
    assert_eq!(streamed_and_complete_calls(&responses).len(), 2);
}

#[tokio::test]
async fn a_malformed_input_streams_as_the_complete_message_keeps_it() {
    let body =
        searched_turn().replace(r#""partial_json":"\"rust 1.95\"}""#, r#""partial_json":"\"rust""#);
    let responses = stream(body).await;
    let calls = streamed_and_complete_calls(&responses);
    assert_eq!(calls[0]["input"], r#"{"query": "rust"#);
}

/// The server tool calls of the partial responses and of the complete message, after
/// checking that the two copies are the same.
fn streamed_and_complete_calls(responses: &[LlmResponse]) -> Vec<Value> {
    let calls: Vec<Value> = responses
        .iter()
        .flat_map(|response| response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::ServerToolCall { server_tool_call } => Some(server_tool_call.clone()),
            _ => None,
        })
        .collect();
    assert!(!responses.last().unwrap().partial);
    assert_eq!(calls.len(), 2, "one streamed copy and one in the complete message: {calls:?}");
    assert_eq!(calls[0], calls[1]);
    calls
}
