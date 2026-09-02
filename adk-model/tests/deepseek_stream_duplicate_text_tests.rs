//! Regression tests for the DeepSeek SSE streaming path.
//!
//! ADK accumulates the parts of *every* chunk a provider yields — partial
//! chunks included (`adk-agent/src/llm_agent.rs`, "Accumulate content for
//! conversation history"). A provider must therefore emit each piece of the
//! response exactly once across the stream.
//!
//! These tests exercise the real streaming code path in
//! `adk-model/src/deepseek/client.rs`: a `wiremock` server returns a DeepSeek
//! streaming (SSE) response, the client parses it, and we assert that
//! concatenating every emitted `Part::Text` reproduces the source text once —
//! not twice.

#![cfg(feature = "deepseek")]

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ThinkingMode};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_client(base_url: &str) -> DeepSeekClient {
    let config = DeepSeekConfig::new("test-key", "deepseek-chat")
        .with_base_url(base_url)
        .with_thinking_mode(ThinkingMode::Disabled);
    DeepSeekClient::new(config).expect("client creation should succeed")
}

fn make_request() -> LlmRequest {
    LlmRequest::new("deepseek-chat", vec![Content::new("user").with_text("Hello")])
}

/// One SSE `chat.completion.chunk` carrying a `delta.content` fragment.
fn content_chunk(content: &str) -> String {
    let v = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deepseek-chat",
        "choices": [{
            "index": 0,
            "delta": { "content": content },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", serde_json::to_string(&v).unwrap())
}

/// The terminal chunk: empty delta plus `finish_reason` and usage.
fn finish_chunk() -> String {
    let v = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deepseek-chat",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10 }
    });
    format!("data: {}\n\n", serde_json::to_string(&v).unwrap())
}

fn sse_body(fragments: &[&str]) -> String {
    let mut body = String::new();
    for f in fragments {
        body.push_str(&content_chunk(f));
    }
    body.push_str(&finish_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

/// Concatenate every `Part::Text` across every chunk of the stream, the way
/// `LlmAgent` accumulates them into the final content.
async fn collect_text(body: String) -> String {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream")
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = make_client(&server.uri());
    let mut stream =
        client.generate_content(make_request(), true).await.expect("stream should start");

    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk should not error");
        if let Some(content) = chunk.content {
            for part in content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(&t);
                }
            }
        }
    }
    text
}

#[tokio::test]
async fn streamed_text_is_emitted_exactly_once() {
    // The finish chunk used to re-emit the whole accumulated text buffer on top
    // of the deltas it had already yielded, so every consumer that accumulates
    // all chunks saw the response twice.
    let text = collect_text(sse_body(&["Hello", ", ", "world!"])).await;
    assert_eq!(text, "Hello, world!");
}

#[tokio::test]
async fn structured_json_output_stays_parsable() {
    // The concrete symptom: with `output_schema` set, a doubled response is
    // `{...}{...}`, which fails JSON parsing ("trailing characters") and burns
    // every schema-validation retry.
    let text = collect_text(sse_body(&["{\"tags\"", ":[\"jazz\"],", "\"price_min\":50}"])).await;
    assert_eq!(text, "{\"tags\":[\"jazz\"],\"price_min\":50}");
    serde_json::from_str::<serde_json::Value>(&text).expect("streamed JSON should parse");
}

#[tokio::test]
async fn final_content_delta_is_not_dropped() {
    // Some providers put the last token in the same chunk as `finish_reason`.
    // That delta must still be emitted.
    let mut body = content_chunk("partial");
    let v = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deepseek-chat",
        "choices": [{
            "index": 0,
            "delta": { "content": " tail" },
            "finish_reason": "stop"
        }]
    });
    body.push_str(&format!("data: {}\n\n", serde_json::to_string(&v).unwrap()));
    body.push_str("data: [DONE]\n\n");

    assert_eq!(collect_text(body).await, "partial tail");
}

/// Same contract for reasoning: with thinking enabled the deltas are yielded as
/// they arrive, so the finish chunk must not replay the accumulated buffer.
#[tokio::test]
async fn streamed_reasoning_is_emitted_exactly_once() {
    let server = MockServer::start().await;
    let mut body = String::new();
    for r in ["think", "ing"] {
        let v = serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": r },
                "finish_reason": null
            }]
        });
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&v).unwrap()));
    }
    body.push_str(&content_chunk("answer"));
    body.push_str(&finish_chunk());
    body.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(body, "text/event-stream")
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let config = DeepSeekConfig::new("test-key", "deepseek-reasoner")
        .with_base_url(server.uri())
        .with_thinking_mode(ThinkingMode::Enabled);
    let client = DeepSeekClient::new(config).expect("client creation should succeed");
    let mut stream = client
        .generate_content(
            LlmRequest::new("deepseek-reasoner", vec![Content::new("user").with_text("Hi")]),
            true,
        )
        .await
        .expect("stream should start");

    let mut thinking = String::new();
    while let Some(chunk) = stream.next().await {
        for part in chunk.expect("chunk should not error").content.into_iter().flat_map(|c| c.parts)
        {
            if let Part::Thinking { thinking: t, .. } = part {
                thinking.push_str(&t);
            }
        }
    }
    assert_eq!(thinking, "thinking");
}
