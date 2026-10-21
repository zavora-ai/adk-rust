//! A streamed Gemini turn yields each function call once, on the final chunk.
//!
//! The agent repeats every partial chunk's content in the terminal event it persists,
//! so a call yielded on a partial chunk would reach anything counting calls over events
//! twice (spec 3.1 §7.13).

#![cfg(feature = "gemini")]

use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, Part};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "gemini-3.5-flash-lite";

fn sse(events: &[Value]) -> ResponseTemplate {
    let body: String = events.iter().map(|event| format!("data: {event}\r\n\r\n")).collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

fn chunk(parts: Value, finish: Option<&str>) -> Value {
    let mut candidate = json!({ "index": 0, "content": { "role": "model", "parts": parts } });
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    json!({ "candidates": [candidate], "modelVersion": MODEL })
}

fn call(path: &str) -> Value {
    json!({ "functionCall": { "name": "read_file", "args": { "path": path } } })
}

async fn stream(events: &[Value]) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(sse(events)).mount(&server).await;
    let model =
        GeminiModel::new_with_base_url("test-key", MODEL, format!("{}/v1beta/", server.uri()))
            .unwrap()
            .with_retry_config(adk_model::retry::RetryConfig::disabled());
    let request = LlmRequest::new(MODEL, vec![Content::new("user").with_text("Read a and b")]);
    let mut stream = model.generate_content(request, true).await.unwrap();
    let mut responses = Vec::new();
    while let Some(item) = stream.next().await {
        responses.push(item.unwrap());
    }
    responses
}

fn calls(response: &LlmResponse) -> Vec<String> {
    response
        .content
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionCall { args, .. } => args["path"].as_str().map(str::to_string),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_call_streamed_before_the_finish_reason_is_yielded_once_on_the_final_chunk() {
    let responses = stream(&[
        chunk(json!([{ "text": "Reading both." }, call("a")]), None),
        chunk(json!([call("b")]), Some("STOP")),
    ])
    .await;

    let (partials, finals): (Vec<_>, Vec<_>) =
        responses.iter().partition(|response| response.partial);
    assert!(partials.iter().all(|response| calls(response).is_empty()), "{responses:?}");
    assert_eq!(finals.len(), 1);
    assert_eq!(calls(finals[0]), ["a", "b"], "every call once, in stream order");
    assert_eq!(
        (finals[0].finish_reason, finals[0].turn_complete),
        (Some(FinishReason::Stop), false)
    );
    assert_eq!(
        responses.iter().map(|response| calls(response).len()).sum::<usize>(),
        2,
        "a counter over every chunk sees each call exactly once"
    );
    assert_eq!(
        partials
            .iter()
            .flat_map(|response| response.content.iter())
            .flat_map(|c| c.parts.iter())
            .count(),
        1,
        "the partial chunk keeps its text"
    );
}

#[tokio::test]
async fn calls_on_the_final_chunk_alone_are_unchanged() {
    let responses = stream(&[
        chunk(json!([{ "text": "Reading." }]), None),
        chunk(json!([call("a"), call("b")]), Some("STOP")),
    ])
    .await;
    let total: usize = responses.iter().map(|response| calls(response).len()).sum();
    assert_eq!(total, 2);
    assert_eq!(calls(responses.last().unwrap()), ["a", "b"]);
}
