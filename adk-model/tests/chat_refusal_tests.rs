//! A Chat Completions `refusal` (with `content: null`) is the turn's answer text, and the turn
//! finishes with `FinishReason::Safety`, streamed and unary.
#![cfg(feature = "openai")]

use adk_core::{AdkError, Content, FinishReason, Llm, LlmRequest, LlmResponse, Part};
use adk_model::openai::OpenAIClient;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const REFUSAL: &str = "I'm sorry, but I can't help with that request.";

async fn turn(template: ResponseTemplate, stream: bool) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(template).expect(1).mount(&server).await;
    let client = OpenAIClient::compatible("sk-test", format!("{}/v1", server.uri()), "scripted")
        .expect("client");
    let request =
        LlmRequest::new("scripted", vec![Content::new("user").with_text("Help me with this.")]);
    let items: Vec<Result<LlmResponse, AdkError>> =
        client.generate_content(request, stream).await.expect("request").collect().await;
    items.into_iter().map(|item| item.expect("a refusal is not a failure")).collect()
}

fn text(responses: &[LlmResponse]) -> String {
    responses
        .iter()
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect()
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
    })
}

#[tokio::test]
async fn streamed_refusal_is_the_answer_and_finishes_with_safety() {
    let body: String = [
        chunk(json!({"role": "assistant", "content": null, "refusal": ""}), None),
        chunk(json!({"refusal": "I'm sorry, but "}), None),
        chunk(json!({"refusal": "I can't help with that request."}), None),
        chunk(json!({}), Some("stop")),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .chain(["data: [DONE]\n\n".to_string()])
    .collect();
    let responses =
        turn(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"), true).await;
    assert_eq!(text(&responses), REFUSAL);
    let last = responses.last().expect("a final response");
    assert!(!last.partial && last.turn_complete, "{last:?}");
    assert_eq!(last.finish_reason, Some(FinishReason::Safety));
    assert_eq!(last.error_code, None);
}

#[tokio::test]
async fn unary_refusal_is_the_answer_and_finishes_with_safety() {
    let body = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": null, "refusal": REFUSAL},
            "finish_reason": "stop"
        }]
    });
    let responses = turn(ResponseTemplate::new(200).set_body_json(body), false).await;
    let [response] = responses.as_slice() else { panic!("one response: {responses:?}") };
    let content = response.content.as_ref().expect("the refusal is the content");
    assert_eq!(content.role, "model");
    assert_eq!(content.parts, vec![Part::Text { text: REFUSAL.into() }]);
    assert_eq!(response.finish_reason, Some(FinishReason::Safety));
    assert!(response.turn_complete);
    assert_eq!(response.error_code, None);
}

#[tokio::test]
async fn answer_without_a_refusal_still_finishes_with_stop() {
    let body = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1_760_000_000,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Paris.", "refusal": null},
            "finish_reason": "stop"
        }]
    });
    let responses = turn(ResponseTemplate::new(200).set_body_json(body), false).await;
    assert_eq!(text(&responses), "Paris.");
    assert_eq!(
        responses.last().and_then(|response| response.finish_reason),
        Some(FinishReason::Stop)
    );
}
