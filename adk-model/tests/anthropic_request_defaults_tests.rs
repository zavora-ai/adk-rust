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

/// A reply whose `id` is `id`.
fn reply(id: &str, text: &str) -> Value {
    json!({
        "id": id, "type": "message", "role": "assistant", "model": "claude-sonnet-5-5",
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 1}
    })
}

#[tokio::test]
async fn a_thread_sends_only_new_messages_and_a_refused_continue_falls_back_to_the_conversation() {
    use wiremock::matchers::body_partial_json;
    let server = MockServer::start().await;
    // The second turn's continue is refused, as an expired thread is; everything else answers.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({"thread": {"type": "continue", "previous_message_id": "msg_2"}})))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "error",
            "error": {"type": "not_found_error", "message": "thread not found", "details": {"error_code": "thread_not_found"}}
        })))
        .mount(&server)
        .await;
    for (turn, id) in [(1, "msg_1"), (2, "msg_2"), (3, "msg_3")] {
        let asks = format!("turn {turn}");
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
                body["messages"]
                    .as_array()
                    .and_then(|messages| messages.last())
                    .is_some_and(|last| last["content"][0]["text"] == asks.as_str())
            })
            .respond_with(
                ResponseTemplate::new(200).set_body_json(reply(id, &format!("answer {turn}"))),
            )
            .mount(&server)
            .await;
    }
    let client = AnthropicClient::new(
        AnthropicConfig::new("test", "claude-sonnet-5-5")
            .with_base_url(server.uri())
            .with_message_threads(true),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let mut history = vec![Content::new("system").with_text("You plan trips.")];
    for turn in 1..=3 {
        history.push(Content::new("user").with_text(format!("turn {turn}")));
        let request = LlmRequest::new("ignored", history.clone());
        let responses = client.generate_content(request, false).await.unwrap();
        let responses: Vec<_> = responses.try_collect().await.unwrap();
        history.push(responses.into_iter().find_map(|response| response.content).unwrap());
    }

    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    let shape = |body: &Value| {
        (
            body["thread"].clone(),
            body["messages"].as_array().unwrap().len(),
            body.get("system").is_some(),
        )
    };
    assert_eq!(
        sent.iter().map(shape).collect::<Vec<_>>(),
        vec![
            // Turn 1 creates the thread with the whole conversation.
            (json!({"type": "create"}), 1, true),
            // Turn 2 continues it with only the new user message.
            (json!({"type": "continue", "previous_message_id": "msg_1"}), 1, false),
            // Turn 3's continue is refused; the conversation goes again without a thread.
            (json!({"type": "continue", "previous_message_id": "msg_2"}), 1, false),
            (Value::Null, 5, true),
        ]
    );
    let betas: Vec<String> = server.received_requests().await.unwrap()[1]
        .headers
        .get("anthropic-beta")
        .map(|value| value.to_str().unwrap().to_string())
        .into_iter()
        .collect();
    assert!(betas[0].contains("message-threads-2026-08-12"), "{betas:?}");
}
