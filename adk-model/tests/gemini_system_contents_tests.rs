//! System-role contents reach Gemini `generateContent` as leading user turns.
//!
//! `LlmAgent` sends instructions as `system` contents. The standard Gemini
//! transport keeps them as user turns so requests stay compatible with
//! context caching, which already carries a system instruction.

use adk_core::{Content, Llm, LlmRequest};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn system_contents_are_sent_as_leading_user_turns() {
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
    let request = LlmRequest::new(
        "gemini-3.7-flash",
        vec![
            Content::new("system").with_text("You are a careful assistant."),
            Content::new("user").with_text("Hello"),
        ],
    );

    let mut stream = model.generate_content(request, false).await.unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        body["contents"],
        json!([
            {"role": "user", "parts": [{"text": "You are a careful assistant."}]},
            {"role": "user", "parts": [{"text": "Hello"}]}
        ])
    );
    assert!(body.get("systemInstruction").is_none() && body.get("system_instruction").is_none());
}
