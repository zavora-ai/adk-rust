//! System-role contents reach Gemini `generateContent` as its `systemInstruction`.
//!
//! `LlmAgent` sends instructions as `system` contents. The standard Gemini
//! transport sends them as `systemInstruction`, except on a context cache, which
//! already carries a system instruction: there they stay leading user turns.

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Sends `request` to a mock `generateContent` endpoint and returns the body it received.
async fn sent_body(request: LlmRequest) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "ok"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1, "totalTokenCount": 4}
        })))
        .mount(&server)
        .await;

    let model = GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.7-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();

    let mut stream = model.generate_content(request, false).await.unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    serde_json::from_slice(&received[0].body).unwrap()
}

fn request() -> LlmRequest {
    LlmRequest::new(
        "gemini-3.7-flash",
        vec![
            Content::new("system").with_text("You are a careful assistant."),
            Content::new("user").with_text("Hello"),
        ],
    )
}

#[tokio::test]
async fn system_contents_are_sent_as_the_system_instruction() {
    let body = sent_body(request()).await;

    assert_eq!(
        body["systemInstruction"],
        json!({"parts": [{"text": "You are a careful assistant."}]})
    );
    assert_eq!(body["contents"], json!([{"role": "user", "parts": [{"text": "Hello"}]}]));
}

#[tokio::test]
async fn system_contents_stay_leading_user_turns_on_a_context_cache() {
    let config = GenerateContentConfig {
        cached_content: Some("cachedContents/abc123".to_string()),
        ..Default::default()
    };

    let body = sent_body(request().with_config(config)).await;

    assert_eq!(
        body["contents"],
        json!([
            {"role": "user", "parts": [{"text": "You are a careful assistant."}]},
            {"role": "user", "parts": [{"text": "Hello"}]}
        ])
    );
    assert_eq!(body["cachedContent"], "cachedContents/abc123");
    assert!(body.get("systemInstruction").is_none());
}
