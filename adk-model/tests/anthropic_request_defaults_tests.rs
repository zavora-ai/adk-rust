//! Request defaults of the Anthropic client: a model-aware `max_tokens` and a
//! configurable request timeout that is generous for non-streaming calls.
#![cfg(feature = "anthropic")]

use std::time::Duration;

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, ThinkingMode};
use adk_model::retry::RetryConfig;
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn message_body() -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-sonnet-5",
        "content": [{"type": "text", "text": "Done."}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 1}
    })
}

async fn send(
    server: &MockServer,
    config: AnthropicConfig,
) -> Result<Vec<Part>, adk_core::AdkError> {
    let client = AnthropicClient::new(config.with_base_url(server.uri()))
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
    let request = LlmRequest::new("ignored", vec![Content::new("user").with_text("Hello")]);
    let responses = client.generate_content(request, false).await?.try_collect::<Vec<_>>().await?;
    Ok(responses
        .into_iter()
        .filter_map(|response| response.content)
        .flat_map(|c| c.parts)
        .collect())
}

#[tokio::test]
async fn always_thinking_models_get_room_beyond_4096_output_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_body()))
        .mount(&server)
        .await;

    send(
        &server,
        AnthropicConfig::new("test", "claude-sonnet-5").with_thinking_mode(ThinkingMode::Adaptive),
    )
    .await
    .unwrap();
    send(&server, AnthropicConfig::new("test", "claude-sonnet-5").with_max_tokens(2_048))
        .await
        .unwrap();

    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["max_tokens"].clone()
        })
        .collect();
    assert_eq!(sent, vec![json!(32_000), json!(2_048)]);
}

#[tokio::test]
async fn non_streaming_requests_wait_for_the_configured_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(message_body())
                .set_delay(Duration::from_millis(1_500)),
        )
        .mount(&server)
        .await;

    let short = AnthropicConfig::new("test", "claude-sonnet-5")
        .with_request_timeout(Duration::from_secs(1));
    let error = send(&server, short).await.expect_err("a 1s timeout cannot wait 1.5s");
    assert!(error.message.to_lowercase().contains("timed out"), "unexpected error: {error}");

    let parts = send(&server, AnthropicConfig::new("test", "claude-sonnet-5")).await.unwrap();
    assert_eq!(parts, vec![Part::Text { text: "Done.".into() }]);
}
