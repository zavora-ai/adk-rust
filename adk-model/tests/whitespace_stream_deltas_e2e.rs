//! End-to-end tests for whitespace preservation through the OpenAI-compatible
//! SSE streaming path.
//!
//! Spec: `preserve-whitespace-stream-deltas`, task 4.4.
//!
//! These tests exercise the *real* streaming code path in
//! `adk-model/src/openai_compatible.rs`: a `wiremock` server returns an OpenAI
//! streaming (SSE) response whose `data:` chunks carry `delta.content`
//! fragments — including whitespace-only chunks and tool-call markup split
//! across chunks. The client parses the SSE stream and routes every text delta
//! through `ToolCallBuffer`, yielding `LlmResponse`s. We assert that the
//! concatenation of every emitted `Part::Text` reproduces the visible
//! (non-tool-call) source text byte for byte, and that tool-call markup is
//! still parsed into `Part::FunctionCall` with surrounding whitespace
//! preserved.
//!
//! SSE payloads follow the real OpenAI streaming chunk format:
//! `data: {"choices":[{"delta":{"content":"..."}}]}\n\n` terminated by
//! `data: [DONE]\n\n`.
//!
//! **Validates: Requirements 2.6, 3.2, 3.3**

#![cfg(feature = "openai")]

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Create an `OpenAICompatible` client pointed at the mock server.
fn make_client(base_url: &str) -> OpenAICompatible {
    let config = OpenAICompatibleConfig::new("test-key", "gpt-4o")
        .with_provider_name("test")
        .with_base_url(base_url);
    OpenAICompatible::new(config).expect("client creation should succeed")
}

/// Build a minimal `LlmRequest`.
fn make_request() -> LlmRequest {
    LlmRequest::new("gpt-4o", vec![Content::new("user").with_text("Hello")])
}

/// Build one SSE `chat.completion.chunk` carrying a `delta.content` fragment.
///
/// Uses `serde_json` so newlines/quotes inside `content` are correctly escaped
/// into a single-line `data:` frame — matching the real OpenAI wire format.
fn content_chunk(content: &str) -> String {
    let v = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": { "content": content },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", serde_json::to_string(&v).unwrap())
}

/// Build the terminal finish chunk (empty delta, `finish_reason: "stop"`).
fn finish_chunk() -> String {
    let v = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10 }
    });
    format!("data: {}\n\n", serde_json::to_string(&v).unwrap())
}

/// Assemble a full SSE body from content fragments plus finish + `[DONE]`.
fn sse_body(contents: &[&str]) -> String {
    let mut body = String::new();
    for c in contents {
        body.push_str(&content_chunk(c));
    }
    body.push_str(&finish_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

/// Mount the SSE body on the mock server and drive the real streaming path,
/// returning every yielded `LlmResponse`.
async fn run_stream(contents: &[&str]) -> Vec<adk_core::LlmResponse> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(contents))
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = make_client(&server.uri());
    let stream_result = client.generate_content(make_request(), true).await;
    assert!(stream_result.is_ok(), "generate_content should not error");

    let mut stream = stream_result.unwrap();
    let mut responses = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(resp) => responses.push(resp),
            Err(e) => panic!("stream yielded error: {e}"),
        }
    }
    responses
}

/// Concatenate every `Part::Text` across all responses, in order.
fn concat_text(responses: &[adk_core::LlmResponse]) -> String {
    responses
        .iter()
        .filter_map(|r| r.content.as_ref())
        .flat_map(|c| &c.parts)
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Collect every `Part::FunctionCall` (name, args, id) across all responses.
fn collect_function_calls(
    responses: &[adk_core::LlmResponse],
) -> Vec<(String, serde_json::Value, Option<String>)> {
    responses
        .iter()
        .filter_map(|r| r.content.as_ref())
        .flat_map(|c| &c.parts)
        .filter_map(|p| match p {
            Part::FunctionCall { name, args, id, .. } => {
                Some((name.clone(), args.clone(), id.clone()))
            }
            _ => None,
        })
        .collect()
}

// ── Test 1: whitespace-only content chunks preserved end-to-end ──────────
//
// **Validates: Requirements 2.6**
//
// The Markdown-structure counterexample from the design streamed through the
// real SSE path. The `"\n\n"` and `"\n"` chunks are whitespace-only deltas —
// on the unfixed buffer they are dropped, collapsing the text to
// `Heading- first- second`. After the fix, the concatenated `Part::Text`
// reconstructs the visible source byte for byte.
#[tokio::test]
async fn sse_whitespace_only_chunks_preserved() {
    let contents = ["Heading", "\n\n", "- first", "\n", "- second"];
    let responses = run_stream(&contents).await;

    let reconstructed = concat_text(&responses);
    assert_eq!(
        reconstructed, "Heading\n\n- first\n- second",
        "whitespace-only SSE content chunks must be preserved; \
         concatenated Part::Text should reconstruct the visible text byte-exactly"
    );
}

// ── Test 2: single-space separation preserved end-to-end ─────────────────
//
// **Validates: Requirements 2.6**
//
// The single-space counterexample: `["Hello", " ", "world"]`. The `" "` chunk
// is a whitespace-only delta that the unfixed buffer drops, yielding
// `Helloworld`. After the fix it reconstructs `Hello world`.
#[tokio::test]
async fn sse_single_space_separation_preserved() {
    let contents = ["Hello", " ", "world"];
    let responses = run_stream(&contents).await;

    let reconstructed = concat_text(&responses);
    assert_eq!(reconstructed, "Hello world", "single-space SSE content chunk must be preserved");
}

// ── Test 3: mixed stream — tool call split across SSE chunks ─────────────
//
// **Validates: Requirements 2.6, 3.2, 3.3**
//
// A Qwen/Hermes `<tool_call>...</tool_call>` markup is split across several
// SSE `delta.content` chunks, with visible text (including whitespace-only
// chunks) before and after. The client must:
//   1. still parse the split markup into a `Part::FunctionCall` (Req 3.2, 3.3),
//   2. preserve the surrounding visible whitespace so the concatenated
//      `Part::Text` reproduces the visible (non-markup) source text (Req 2.6).
#[tokio::test]
async fn sse_mixed_stream_toolcall_split_preserves_whitespace() {
    // Visible text is everything outside the <tool_call>...</tool_call> markup.
    // Expected visible reconstruction: "Let me search.\n\nDone."
    let contents = [
        "Let me search.",
        "\n",
        // Tool-call markup split across chunks.
        "<tool_call>",
        "{\"name\": \"search\", ",
        "\"arguments\": {\"q\": \"rust\"}}",
        "</tool_call>",
        "\n",
        "Done.",
    ];
    let responses = run_stream(&contents).await;

    // 1. Tool call still produced from the split markup.
    let calls = collect_function_calls(&responses);
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one Part::FunctionCall from the split tool-call markup, got {}",
        calls.len()
    );
    let (name, args, _id) = &calls[0];
    assert_eq!(name, "search", "tool call name should be parsed as 'search'");
    assert_eq!(args["q"], "rust", "tool call args should contain q=rust");

    // 2. Surrounding visible whitespace preserved (markup bytes excluded).
    let reconstructed = concat_text(&responses);
    assert_eq!(
        reconstructed, "Let me search.\n\nDone.",
        "visible whitespace around split tool-call markup must be preserved; \
         concatenated Part::Text should reconstruct the non-markup source text"
    );
}
