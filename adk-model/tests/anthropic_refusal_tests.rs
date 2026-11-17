//! A classifier refusal (`stop_reason: "refusal"`) ends the turn with `FinishReason::Refusal`
//! and an error naming the refusal category and explanation from `stop_details`, streamed
//! and unary.
#![cfg(feature = "anthropic")]

use adk_core::{AdkError, Content, FinishReason, Llm, LlmRequest, LlmResponse};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EXPLANATION: &str = "This request touches on a restricted cybersecurity topic.";

fn stop_details() -> serde_json::Value {
    json!({"type": "refusal", "category": "cyber", "explanation": EXPLANATION})
}

fn unary_body() -> serde_json::Value {
    json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-5-5",
        "content": [{"type": "text", "text": "I can"}],
        "stop_reason": "refusal",
        "stop_sequence": null,
        "stop_details": stop_details(),
        "usage": {"input_tokens": 12, "output_tokens": 3}
    })
}

fn sse_body() -> String {
    let events = [
        json!({"type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 12, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "I can"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "refusal", "stop_sequence": null, "stop_details": stop_details()},
            "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    events
        .iter()
        .map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()))
        .collect()
}

async fn responses(stream: bool) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    let template = if stream {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse_body())
    } else {
        ResponseTemplate::new(200).set_body_json(unary_body())
    };
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(template)
        .expect(1)
        .mount(&server)
        .await;
    let client = AnthropicClient::new(
        AnthropicConfig::new("test-key", "claude-sonnet-5-5").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let request =
        LlmRequest::new("claude-sonnet-5-5", vec![Content::new("user").with_text("Explain.")]);
    let items: Vec<Result<LlmResponse, AdkError>> =
        client.generate_content(request, stream).await.expect("request").collect().await;
    items.into_iter().map(|item| item.expect("a refusal arrives as a response")).collect()
}

#[tokio::test]
async fn a_refusal_ends_the_turn_with_its_category_and_explanation() {
    for stream in [false, true] {
        let responses = responses(stream).await;
        let last = responses.last().expect("final response");
        assert_eq!(last.finish_reason, Some(FinishReason::Refusal), "stream={stream}");
        assert_eq!(last.error_code.as_deref(), Some("refusal"), "stream={stream}");
        assert_eq!(
            last.error_message.as_deref(),
            Some(format!("Claude declined the request (category cyber): {EXPLANATION}").as_str()),
            "stream={stream}"
        );
        assert_eq!(
            last.provider_metadata.as_ref().and_then(|metadata| metadata.get("stop_details")),
            Some(&stop_details()),
            "stream={stream}"
        );
    }
}
