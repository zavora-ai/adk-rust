//! Streams that end before their terminal event, end mid tool call, or carry an error
//! frame fail instead of returning a truncated success, and `data:` fields without a space
//! or UTF-8 characters split across network writes decode exactly.
//!
//! The upstream is a raw HTTP server that sends each scripted write as its own chunk, so a
//! test controls where the client sees a network boundary.
#![cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "groq",
    feature = "ollama",
    feature = "openrouter"
))]

use adk_core::{AdkError, Content, Llm, LlmRequest, LlmResponse, Part};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves every request with `writes`, each as a separate HTTP chunk, then ends the
/// response cleanly, as a proxy does when the upstream stopped early.
async fn serve(content_type: &'static str, writes: Vec<Vec<u8>>) -> String {
    serve_tail(content_type, writes, Some(b"0\r\n\r\n")).await.0
}

async fn serve_tail(
    content_type: &'static str,
    writes: Vec<Vec<u8>>,
    ending: Option<&'static [u8]>,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let writes = Arc::new(writes);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let writes = writes.clone();
            let calls = observed.clone();
            tokio::spawn(async move {
                let mut received = Vec::new();
                let mut buffer = [0_u8; 4096];
                let header_end = loop {
                    let Ok(read) = socket.read(&mut buffer).await else { return };
                    if read == 0 {
                        return;
                    }
                    received.extend_from_slice(&buffer[..read]);
                    if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break end + 4;
                    }
                };
                let head = String::from_utf8_lossy(&received[..header_end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while received.len() < header_end + length {
                    let Ok(read) = socket.read(&mut buffer).await else { return };
                    if read == 0 {
                        return;
                    }
                    received.extend_from_slice(&buffer[..read]);
                }
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let header = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
                );
                if socket.write_all(header.as_bytes()).await.is_err() {
                    return;
                }
                for write in writes.iter() {
                    let mut frame = format!("{:x}\r\n", write.len()).into_bytes();
                    frame.extend_from_slice(write);
                    frame.extend_from_slice(b"\r\n");
                    if socket.write_all(&frame).await.is_err() || socket.flush().await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
                if let Some(ending) = ending {
                    let _ = socket.write_all(ending).await;
                } else {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    (base, calls)
}

/// Splits `bytes` in the middle of the first occurrence of `needle`; a write without it
/// stays whole.
#[cfg(any(feature = "groq", feature = "ollama", feature = "openrouter"))]
fn split_inside(bytes: Vec<u8>, needle: &str) -> Vec<Vec<u8>> {
    let needle = needle.as_bytes();
    match bytes.windows(needle.len()).position(|window| window == needle) {
        Some(start) => {
            let split = start + needle.len() / 2;
            vec![bytes[..split].to_vec(), bytes[split..].to_vec()]
        }
        None => vec![bytes],
    }
}

fn request(model: &str) -> LlmRequest {
    LlmRequest::new(model, vec![Content::new("user").with_text("What is the capital of France?")])
}

/// Collects every response and the error that ended the stream, if any.
async fn collect(
    model: &dyn Llm,
    request: LlmRequest,
    stream: bool,
) -> (Vec<LlmResponse>, Option<AdkError>) {
    let mut responses = Vec::new();
    let mut stream = match model.generate_content(request, stream).await {
        Ok(stream) => stream,
        Err(error) => return (responses, Some(error)),
    };
    while let Some(item) = stream.next().await {
        match item {
            Ok(response) => responses.push(response),
            Err(error) => return (responses, Some(error)),
        }
    }
    (responses, None)
}

fn text(responses: &[LlmResponse]) -> String {
    responses
        .iter()
        .filter(|response| response.partial)
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect()
}

fn function_calls(responses: &[LlmResponse]) -> usize {
    responses
        .iter()
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter(|part| matches!(part, Part::FunctionCall { .. }))
        .count()
}

/// Asserts the stream failed with `code` and yielded neither a tool call nor a final
/// response, and returns what it yielded before failing.
#[track_caller]
fn assert_fails(
    outcome: (Vec<LlmResponse>, Option<AdkError>),
    code: &str,
) -> (Vec<LlmResponse>, AdkError) {
    let (responses, error) = outcome;
    let Some(error) = error else { panic!("expected {code}, got Ok {responses:?}") };
    assert_eq!(error.code, code, "{error}");
    assert_eq!(function_calls(&responses), 0, "a failed stream yielded a tool call");
    assert!(
        responses.iter().all(|response| response.partial),
        "a failed stream yielded a final response: {responses:?}"
    );
    (responses, error)
}

/// Chat Completions chunks, shared by the OpenAI-compatible, DeepSeek, Azure AI, Groq, and
/// OpenRouter Chat wires.
#[cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "groq",
    feature = "openrouter"
))]
mod chat {
    use serde_json::{Value, json};

    pub fn sse(lines: &[Value]) -> Vec<Vec<u8>> {
        lines.iter().map(|value| format!("data: {value}\n\n").into_bytes()).collect()
    }

    pub fn done() -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    pub fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "created": 1_760_000_000,
            "model": "scripted",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        })
    }

    pub fn text_chunk(text: &str) -> Value {
        chunk(json!({"content": text}), None)
    }

    pub fn tool_call_chunks(arguments: &[&str]) -> Vec<Value> {
        let mut chunks = vec![chunk(
            json!({"role": "assistant", "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": {"name": "get_weather", "arguments": ""},
            }]}),
            None,
        )];
        chunks.extend(arguments.iter().map(|fragment| {
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": fragment}}]}), None)
        }));
        chunks
    }

    #[cfg(any(feature = "openai", feature = "deepseek", feature = "azure-ai", feature = "groq"))]
    pub fn openai_error() -> Value {
        json!({"error": {"message": "The server had an error.", "type": "server_error", "code": "server_error"}})
    }

    pub fn finished_text(texts: &[&str]) -> Vec<Vec<u8>> {
        let mut chunks: Vec<Value> = texts.iter().map(|text| text_chunk(text)).collect();
        chunks.push(chunk(json!({}), Some("stop")));
        let mut writes = sse(&chunks);
        writes.push(done());
        writes
    }
}

#[cfg(feature = "openai")]
mod openai_compatible {
    use super::chat::*;
    use super::*;
    use adk_core::{ErrorCategory, FinishReason};
    use adk_model::openai::OpenAIClient;
    use serde_json::json;

    async fn client(writes: Vec<Vec<u8>>) -> OpenAIClient {
        let base = serve("text/event-stream", writes).await;
        OpenAIClient::compatible("sk-test", format!("{base}/v1"), "scripted").expect("client")
    }

    #[tokio::test]
    async fn stream_without_a_finish_reason_or_done_fails() {
        let model =
            client(sse(&[text_chunk("The capital of France "), text_chunk("is Par")])).await;
        let outcome = collect(&model, request("scripted"), true).await;
        let (responses, error) = assert_fails(outcome, "model.openai_compat.incomplete_stream");
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert!(error.is_retryable());
        assert_eq!(text(&responses), "The capital of France is Par");
    }

    #[tokio::test]
    async fn stream_cut_mid_tool_call_fails() {
        let model = client(sse(&tool_call_chunks(&[r#"{"city":"Par"#]))).await;
        let outcome = collect(&model, request("scripted"), true).await;
        assert_fails(outcome, "model.openai_compat.incomplete_stream");
    }

    #[tokio::test]
    async fn done_with_unfinished_tool_calls_fails() {
        let mut writes = sse(&tool_call_chunks(&[r#"{"city":"Paris"}"#]));
        writes.push(done());
        let outcome = collect(&client(writes).await, request("scripted"), true).await;
        assert_fails(outcome, "model.openai_compat.incomplete_stream");
    }

    #[tokio::test]
    async fn error_chunk_fails_even_when_done_follows() {
        let mut writes = sse(&[text_chunk("The capital of France "), openai_error()]);
        writes.push(done());
        let outcome = collect(&client(writes).await, request("scripted"), true).await;
        let (_, error) = assert_fails(outcome, "model.openai_compat.stream_error");
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert!(error.message.contains("The server had an error."), "{error}");
        assert_eq!(error.details.metadata["provider_error_code"], "server_error");
    }

    #[tokio::test]
    async fn error_finish_reason_fails() {
        let mut writes = sse(&[text_chunk("The capital"), chunk(json!({}), Some("error"))]);
        writes.push(done());
        let outcome = collect(&client(writes).await, request("scripted"), true).await;
        assert_fails(outcome, "model.openai_compat.stream_error");
    }

    #[tokio::test]
    async fn unknown_finish_reason_is_other() {
        let mut writes = sse(&[text_chunk("Paris."), chunk(json!({}), Some("end_turn"))]);
        writes.push(done());
        let (responses, error) = collect(&client(writes).await, request("scripted"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            responses.last().and_then(|response| response.finish_reason),
            Some(FinishReason::Other)
        );
    }

    #[tokio::test]
    async fn data_fields_without_a_space_decode() {
        let writes = finished_text(&["The capital of France ", "is Paris."])
            .into_iter()
            .map(|write| {
                String::from_utf8(write).expect("utf8").replacen("data: ", "data:", 1).into_bytes()
            })
            .collect();
        let (responses, error) = collect(&client(writes).await, request("scripted"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), "The capital of France is Paris.");
        assert_eq!(
            responses.last().and_then(|response| response.finish_reason),
            Some(FinishReason::Stop)
        );
    }

    #[tokio::test]
    async fn last_line_without_a_line_ending_is_read() {
        let mut writes = sse(&[text_chunk("Paris.")]);
        writes.push(b"data: [DONE]".to_vec());
        let (responses, error) = collect(&client(writes).await, request("scripted"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), "Paris.");
    }

    #[tokio::test]
    async fn non_streaming_error_body_fails() {
        let model = client(vec![openai_error().to_string().into_bytes()]).await;
        let outcome = collect(&model, request("scripted"), false).await;
        let error = outcome.1.expect("an error body must fail");
        assert_eq!(error.code, "model.openai_compat.stream_error");
    }
}

/// DeepSeek and Azure AI report a stream's failures with the same codes and categories as
/// the other Chat Completions clients.
#[cfg(any(feature = "deepseek", feature = "azure-ai"))]
macro_rules! chat_stream_failures {
    ($provider:ident, $prefix:literal, $client:expr) => {
        mod $provider {
            use super::chat::*;
            use super::*;
            use adk_core::{ErrorCategory, FinishReason};

            #[tokio::test]
            async fn complete_stream_succeeds() {
                let writes = finished_text(&["The capital of France ", "is Paris."]);
                let base = serve("text/event-stream", writes).await;
                let (responses, error) = collect(&$client(base), request("scripted"), true).await;
                assert!(error.is_none(), "{error:?}");
                assert_eq!(text(&responses), "The capital of France is Paris.");
                assert_eq!(
                    responses.last().and_then(|response| response.finish_reason),
                    Some(FinishReason::Stop)
                );
            }

            #[tokio::test]
            async fn error_chunk_fails_with_the_provider_message_and_category() {
                let mut writes = sse(&[text_chunk("The capital of France "), openai_error()]);
                writes.push(done());
                let base = serve("text/event-stream", writes).await;
                let outcome = collect(&$client(base), request("scripted"), true).await;
                let (responses, error) = assert_fails(outcome, concat!($prefix, ".stream_error"));
                assert_eq!(text(&responses), "The capital of France ");
                assert_eq!(error.category, ErrorCategory::Unavailable);
                assert!(error.is_retryable());
                assert!(error.message.contains("The server had an error."), "{error}");
                assert_eq!(error.details.metadata["provider_error_code"], "server_error");
            }

            #[tokio::test]
            async fn stream_without_a_finish_reason_fails() {
                let writes = sse(&[text_chunk("The capital of France "), text_chunk("is Par")]);
                let base = serve("text/event-stream", writes).await;
                let outcome = collect(&$client(base), request("scripted"), true).await;
                let (_, error) = assert_fails(outcome, concat!($prefix, ".incomplete_stream"));
                assert_eq!(error.category, ErrorCategory::Unavailable);
                assert!(error.is_retryable());
            }

            #[tokio::test]
            async fn stream_cut_mid_tool_call_fails() {
                let base =
                    serve("text/event-stream", sse(&tool_call_chunks(&[r#"{"city":"Par"#]))).await;
                let outcome = collect(&$client(base), request("scripted"), true).await;
                assert_fails(outcome, concat!($prefix, ".incomplete_stream"));
            }

            #[tokio::test]
            async fn chunk_that_does_not_decode_fails_without_quoting_it() {
                let mut writes = sse(&[text_chunk("The capital of France ")]);
                writes.push(b"data: {\"choices\": [{\"delta\": \"Paris\n\n".to_vec());
                let base = serve("text/event-stream", writes).await;
                let outcome = collect(&$client(base), request("scripted"), true).await;
                let (_, error) = assert_fails(outcome, concat!($prefix, ".invalid_chunk"));
                assert!(!error.message.contains("Paris"), "{error}");
            }
        }
    };
}

#[cfg(feature = "deepseek")]
chat_stream_failures!(deepseek, "model.deepseek", |base: String| {
    adk_model::deepseek::DeepSeekClient::new(
        adk_model::deepseek::DeepSeekConfig::new("sk-test", "deepseek-flash").with_base_url(base),
    )
    .expect("client")
});

#[cfg(feature = "azure-ai")]
chat_stream_failures!(azure_ai, "model.azure_ai", |base: String| {
    adk_model::azure_ai::AzureAIClient::new(adk_model::azure_ai::AzureAIConfig::new(
        base,
        "azure-test",
        "mistral-large",
    ))
    .expect("client")
});

#[cfg(feature = "groq")]
mod groq {
    use super::chat::*;
    use super::*;
    use adk_model::groq::{GroqClient, GroqConfig};
    use serde_json::json;

    async fn client(writes: Vec<Vec<u8>>) -> GroqClient {
        let base = serve("text/event-stream", writes).await;
        GroqClient::new(GroqConfig::new("gsk-test", "openai/gpt-oss-120b").with_base_url(base))
            .expect("client")
    }

    #[tokio::test]
    async fn stream_without_a_finish_reason_or_done_fails() {
        let model =
            client(sse(&[text_chunk("The capital of France "), text_chunk("is Par")])).await;
        let outcome = collect(&model, request("openai/gpt-oss-120b"), true).await;
        assert_fails(outcome, "model.groq.incomplete_stream");
    }

    #[tokio::test]
    async fn stream_cut_mid_tool_call_fails() {
        let model = client(sse(&tool_call_chunks(&[r#"{"city":"Par"#]))).await;
        let outcome = collect(&model, request("openai/gpt-oss-120b"), true).await;
        assert_fails(outcome, "model.groq.incomplete_stream");
    }

    #[tokio::test]
    async fn error_chunk_fails_even_when_done_follows() {
        let mut writes = sse(&[text_chunk("The capital of France "), openai_error()]);
        writes.push(done());
        let outcome = collect(&client(writes).await, request("openai/gpt-oss-120b"), true).await;
        assert_fails(outcome, "model.groq.stream_error");
    }

    #[tokio::test]
    async fn finish_chunk_that_does_not_decode_fails_instead_of_dropping_the_tool_call() {
        let mut chunks = tool_call_chunks(&[r#"{"city":"Paris"}"#]);
        let mut finish = chunk(json!({}), Some("tool_calls"));
        finish["choices"][0]["finish_reason"] = json!(42);
        chunks.push(finish);
        let mut writes = sse(&chunks);
        writes.push(done());
        let outcome = collect(&client(writes).await, request("openai/gpt-oss-120b"), true).await;
        let (_, error) = assert_fails(outcome, "model.groq.invalid_chunk");
        assert!(!error.message.contains("Paris"), "{error}");
    }

    #[tokio::test]
    async fn finish_chunk_with_null_usage_counts_keeps_the_tool_call() {
        let mut chunks = tool_call_chunks(&[r#"{"city":"Paris"}"#]);
        let mut finish = chunk(json!({}), Some("tool_calls"));
        finish["usage"] =
            json!({"prompt_tokens": 12, "completion_tokens": null, "total_tokens": null});
        chunks.push(finish);
        let mut writes = sse(&chunks);
        writes.push(done());
        let (responses, error) =
            collect(&client(writes).await, request("openai/gpt-oss-120b"), true).await;
        assert!(error.is_none(), "{error:?}");
        let call = responses
            .iter()
            .find(|response| response.content.as_ref().is_some_and(Content::has_function_calls))
            .expect("the tool call reaches the agent");
        let usage = call.usage_metadata.as_ref().expect("the call carries the usage");
        assert_eq!((usage.prompt_token_count, usage.total_token_count), (12, 12));
    }

    #[tokio::test]
    async fn utf8_split_across_network_writes_is_exact() {
        let answer = "Café ☕ in 東京 🙂.";
        let writes = finished_text(&[answer])
            .into_iter()
            .flat_map(|write| split_inside(write, "🙂"))
            .collect();
        let (responses, error) =
            collect(&client(writes).await, request("openai/gpt-oss-120b"), true).await;
        assert!(error.is_none(), "{error:?}");
        let final_text: String = responses
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .collect();
        assert_eq!(final_text, answer);
    }

    #[tokio::test]
    async fn data_fields_without_a_space_decode() {
        let writes = finished_text(&["Paris."])
            .into_iter()
            .map(|write| {
                String::from_utf8(write).expect("utf8").replacen("data: ", "data:", 1).into_bytes()
            })
            .collect();
        let (responses, error) =
            collect(&client(writes).await, request("openai/gpt-oss-120b"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), "Paris.");
    }
}

#[cfg(feature = "ollama")]
mod ollama {
    use super::*;
    use adk_model::ollama::{OllamaConfig, OllamaModel};
    use serde_json::json;

    fn line(content: &str, done: bool) -> Vec<u8> {
        let mut value = json!({
            "model": "qwen3.5",
            "created_at": "2026-10-16T00:00:00Z",
            "message": {"role": "assistant", "content": content},
            "done": done,
        });
        if done {
            for (key, count) in [
                ("total_duration", 10),
                ("load_duration", 1),
                ("prompt_eval_count", 12),
                ("prompt_eval_duration", 1),
                ("eval_count", 7),
                ("eval_duration", 1),
            ] {
                value[key] = json!(count);
            }
        }
        format!("{value}\n").into_bytes()
    }

    async fn client(writes: Vec<Vec<u8>>) -> OllamaModel {
        let base = serve("application/x-ndjson", writes).await;
        OllamaModel::new(OllamaConfig::with_host(base, "qwen3.5")).expect("client")
    }

    #[tokio::test]
    async fn complete_stream_succeeds() {
        let model = client(vec![line("Paris", false), line(".", false), line("", true)]).await;
        let (responses, error) = collect(&model, request("qwen3.5"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), "Paris.");
        assert!(responses.last().is_some_and(|response| response.turn_complete));
    }

    #[tokio::test]
    async fn stream_without_done_fails() {
        let model =
            client(vec![line("The capital of France ", false), line("is Par", false)]).await;
        let outcome = collect(&model, request("qwen3.5"), true).await;
        let (_, error) = assert_fails(outcome, "model.ollama.incomplete_stream");
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn error_line_fails() {
        let model = client(vec![
            line("The capital of France ", false),
            b"{\"error\":\"model runner has unexpectedly stopped\"}\n".to_vec(),
        ])
        .await;
        let outcome = collect(&model, request("qwen3.5"), true).await;
        let (_, error) = assert_fails(outcome, "model.ollama.stream_error");
        assert!(error.message.contains("model runner has unexpectedly stopped"), "{error}");
    }

    #[tokio::test]
    async fn streamed_deepseek_native_tokens_become_a_call() {
        let mut request = request("deepseek-v3");
        request.tools.insert(
            "get_weather".to_string(),
            json!({"name": "get_weather", "description": "Weather", "parameters": {"type": "object"}}),
        );
        let model = client(vec![
            line("<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>get_", false),
            line("weather\n```json\n{\"city\": \"Paris\"}\n```<｜tool▁call▁end｜>", false),
            line("<｜tool▁calls▁end｜>", false),
            line("", true),
        ])
        .await;
        let (responses, error) = collect(&model, request, true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(function_calls(&responses), 1);
        assert!(!text(&responses).contains("tool▁"), "{responses:?}");
    }

    #[tokio::test]
    async fn streamed_tool_calls_are_partial_and_only_done_ends_the_turn() {
        let calls = json!({
            "model": "qwen3.5",
            "created_at": "2026-10-16T00:00:00Z",
            "message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "transfer", "arguments": {"amount": 10}}},
                {"function": {"name": "transfer", "arguments": {"amount": 20}}},
            ]},
            "done": false,
        });
        let model = client(vec![format!("{calls}\n").into_bytes(), line("", true)]).await;
        let (responses, error) = collect(&model, request("qwen3.5"), true).await;
        assert!(error.is_none(), "{error:?}");

        let terminal: Vec<_> = responses.iter().filter(|response| !response.partial).collect();
        assert_eq!(terminal.len(), 1, "one terminal response per turn: {responses:?}");
        assert!(terminal[0].turn_complete);
        let streamed_calls: Vec<_> = responses
            .iter()
            .filter(|response| response.partial)
            .flat_map(|response| response.content.iter().flat_map(|content| content.parts.iter()))
            .filter_map(|part| match part {
                Part::FunctionCall { args, .. } => Some(args["amount"].clone()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed_calls, vec![json!(10), json!(20)]);
    }

    #[tokio::test]
    async fn utf8_split_across_network_writes_is_exact() {
        let answer = "Café ☕ in 東京 🙂.";
        let mut writes = split_inside(line(answer, false), "🙂");
        writes.push(line("", true));
        let (responses, error) = collect(&client(writes).await, request("qwen3.5"), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), answer);
    }
}

#[cfg(feature = "openrouter")]
mod openrouter {
    use super::chat::*;
    use super::*;
    use adk_core::FinishReason;
    use adk_model::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
    use serde_json::{Value, json};

    const MODEL: &str = "openai/gpt-4.1-mini";

    async fn client(
        mode: OpenRouterApiMode,
        content_type: &'static str,
        writes: Vec<Vec<u8>>,
    ) -> OpenRouterClient {
        let base = serve(content_type, writes).await;
        OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", MODEL)
                .with_base_url(base)
                .with_default_api_mode(mode),
        )
        .expect("client")
    }

    async fn chat(writes: Vec<Vec<u8>>) -> OpenRouterClient {
        client(OpenRouterApiMode::ChatCompletions, "text/event-stream", writes).await
    }

    async fn responses(writes: Vec<Vec<u8>>) -> OpenRouterClient {
        client(OpenRouterApiMode::Responses, "text/event-stream", writes).await
    }

    fn event(kind: &str, mut fields: Value) -> Value {
        fields["type"] = json!(kind);
        fields
    }

    fn delta(text: &str) -> Value {
        event(
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0, "delta": text}),
        )
    }

    fn terminal(kind: &str, status: &str, text: &str, extra: Value) -> Value {
        let mut response = json!({
            "id": "resp_1",
            "status": status,
            "output": [{
                "id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": text}],
            }],
            "usage": {"input_tokens": 12, "output_tokens": 7, "total_tokens": 19},
        });
        if let (Some(response), Some(extra)) = (response.as_object_mut(), extra.as_object()) {
            response.extend(extra.clone());
        }
        event(kind, json!({"response": response}))
    }

    #[tokio::test]
    async fn chat_stream_without_a_finish_reason_or_done_fails() {
        let model = chat(sse(&[text_chunk("The capital of France "), text_chunk("is Par")])).await;
        let outcome = collect(&model, request(MODEL), true).await;
        assert_fails(outcome, "model.openrouter.chat_stream_incomplete");
    }

    #[tokio::test]
    async fn chat_stream_cut_mid_tool_call_fails() {
        let model = chat(sse(&tool_call_chunks(&[r#"{"city":"Par"#]))).await;
        let outcome = collect(&model, request(MODEL), true).await;
        assert_fails(outcome, "model.openrouter.chat_stream_incomplete");
    }

    #[tokio::test]
    async fn chat_tool_call_with_invalid_arguments_is_marked_instead_of_dropped() {
        let mut chunks = tool_call_chunks(&[r#"{"city":"Par"#]);
        chunks.push(chunk(json!({}), Some("length")));
        let mut writes = sse(&chunks);
        writes.push(done());
        let (responses, error) = collect(&chat(writes).await, request(MODEL), true).await;
        assert!(error.is_none(), "{error:?}");
        let args = responses
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .find_map(|part| match part {
                Part::FunctionCall { args, .. } => Some(args.clone()),
                _ => None,
            })
            .expect("the call is emitted");
        assert!(adk_core::invalid_tool_arguments_reason(&args).is_some(), "{args}");
    }

    #[tokio::test]
    async fn chat_utf8_split_across_network_writes_is_exact() {
        let answer = "Café ☕ in 東京 🙂.";
        let writes = finished_text(&[answer])
            .into_iter()
            .flat_map(|write| split_inside(write, "🙂"))
            .collect();
        let (responses, error) = collect(&chat(writes).await, request(MODEL), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), answer);
    }

    #[tokio::test]
    async fn responses_stream_without_a_terminal_event_fails() {
        let mut writes = sse(&[delta("The capital of France "), delta("is Par")]);
        writes.push(done());
        let outcome = collect(&responses(writes).await, request(MODEL), true).await;
        let (_, error) = assert_fails(outcome, "model.openrouter.responses_stream_incomplete");
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn responses_stream_cut_mid_tool_call_fails() {
        let writes = sse(&[
            event(
                "response.output_item.added",
                json!({"output_index": 0, "item": {"id": "fc_1", "type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "", "status": "in_progress"}}),
            ),
            event(
                "response.function_call_arguments.delta",
                json!({"item_id": "fc_1", "delta": r#"{"city":"Par"#}),
            ),
        ]);
        let outcome = collect(&responses(writes).await, request(MODEL), true).await;
        assert_fails(outcome, "model.openrouter.responses_stream_incomplete");
    }

    #[tokio::test]
    async fn responses_failed_event_fails() {
        let failed = event(
            "response.failed",
            json!({"response": {"id": "resp_1", "status": "failed", "output": [],
                "error": {"code": "server_error", "message": "The model failed to generate a response."}}}),
        );
        let mut writes = sse(&[delta("The capital of France "), failed]);
        writes.push(done());
        let outcome = collect(&responses(writes).await, request(MODEL), true).await;
        let (_, error) = assert_fails(outcome, "model.openrouter.response_failed");
        assert!(error.message.contains("The model failed to generate a response."), "{error}");
    }

    #[tokio::test]
    async fn responses_incomplete_at_the_output_limit_finishes_with_max_tokens() {
        let mut writes = sse(&[
            delta("One, two, "),
            terminal(
                "response.incomplete",
                "incomplete",
                "One, two, ",
                json!({"incomplete_details": {"reason": "max_output_tokens"}}),
            ),
        ]);
        writes.push(done());
        let (responses, error) = collect(&responses(writes).await, request(MODEL), true).await;
        assert!(error.is_none(), "{error:?}");
        let last = responses.last().expect("terminal response");
        assert_eq!(last.finish_reason, Some(FinishReason::MaxTokens));
        assert!(last.turn_complete && !last.partial);
    }

    #[tokio::test]
    async fn responses_incomplete_for_another_reason_fails() {
        let writes = sse(&[terminal(
            "response.incomplete",
            "incomplete",
            "One, two, ",
            json!({"incomplete_details": {"reason": "interrupted"}}),
        )]);
        let outcome = collect(&responses(writes).await, request(MODEL), true).await;
        assert_fails(outcome, "model.openrouter.response_incomplete");
    }

    #[tokio::test]
    async fn responses_terminal_content_replaces_the_streamed_deltas() {
        let mut writes = sse(&[
            delta("The capital of France "),
            delta("is Paris."),
            terminal(
                "response.completed",
                "completed",
                "The capital of France is Paris.",
                json!({}),
            ),
        ]);
        writes.push(done());
        let (responses, error) = collect(&responses(writes).await, request(MODEL), true).await;
        assert!(error.is_none(), "{error:?}");
        let last = responses.last().expect("terminal response");
        assert_eq!(
            last.provider_metadata.as_ref().and_then(|metadata| metadata.get("content_complete")),
            Some(&json!(true))
        );
        assert_eq!(text(&responses), "The capital of France is Paris.");
    }

    #[tokio::test]
    async fn responses_utf8_split_across_network_writes_is_exact() {
        let answer = "Café ☕ in 東京 🙂.";
        let writes = sse(&[delta(answer)])
            .into_iter()
            .flat_map(|write| split_inside(write, "🙂"))
            .chain(sse(&[terminal("response.completed", "completed", answer, json!({}))]))
            .collect();
        let (responses, error) = collect(&responses(writes).await, request(MODEL), true).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(text(&responses), answer);
    }

    #[tokio::test]
    async fn non_streaming_failed_response_fails() {
        let body = json!({"id": "resp_1", "object": "response", "status": "failed", "output": [],
            "error": {"code": "server_error", "message": "The model failed to generate a response."}});
        let model = client(
            OpenRouterApiMode::Responses,
            "application/json",
            vec![body.to_string().into_bytes()],
        )
        .await;
        let outcome = collect(&model, request(MODEL), false).await;
        let error = outcome.1.expect("a failed response must be an error");
        assert_eq!(error.code, "model.openrouter.response_failed");
    }
}

/// A finish reason the client does not know reads as `Other`, not `Stop`.
#[cfg(any(feature = "groq", feature = "deepseek"))]
mod unknown_finish_reason {
    #[cfg(feature = "deepseek")]
    use super::chat::*;
    use super::*;
    use adk_core::FinishReason;
    use serde_json::json;

    fn completion(reason: &str) -> Vec<Vec<u8>> {
        vec![
            json!({
                "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "scripted",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "Partial"},
                             "finish_reason": reason}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
            })
            .to_string()
            .into_bytes(),
        ]
    }

    #[cfg(feature = "deepseek")]
    fn streamed(reason: &str) -> Vec<Vec<u8>> {
        let mut writes = sse(&[text_chunk("Partial"), chunk(json!({}), Some(reason))]);
        writes.push(done());
        writes
    }

    fn finish_reason(outcome: (Vec<LlmResponse>, Option<AdkError>)) -> Option<FinishReason> {
        let (responses, error) = outcome;
        assert!(error.is_none(), "{error:?}");
        responses.iter().rev().find_map(|response| response.finish_reason)
    }

    #[cfg(feature = "groq")]
    #[tokio::test]
    async fn groq_complete_response() {
        use adk_model::groq::{GroqClient, GroqConfig};
        let base = serve("application/json", completion("model_context_window_exceeded")).await;
        let model = GroqClient::new(GroqConfig::new("gsk-test", "m").with_base_url(base)).unwrap();
        let outcome = collect(&model, request("m"), false).await;
        assert_eq!(finish_reason(outcome), Some(FinishReason::Other));
    }

    #[cfg(feature = "deepseek")]
    #[tokio::test]
    async fn deepseek_complete_and_streamed_responses() {
        use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
        let client = |base: String| {
            DeepSeekClient::new(DeepSeekConfig::new("sk-test", "m").with_base_url(base)).unwrap()
        };
        let reason = "insufficient_system_resource";
        let base = serve("application/json", completion(reason)).await;
        let outcome = collect(&client(base), request("m"), false).await;
        assert_eq!(finish_reason(outcome), Some(FinishReason::Other));
        let base = serve("text/event-stream", streamed(reason)).await;
        let outcome = collect(&client(base), request("m"), true).await;
        assert_eq!(finish_reason(outcome), Some(FinishReason::Other));
    }
}

#[cfg(any(feature = "openai", feature = "deepseek", feature = "gemini"))]
mod terminal_usage {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[derive(Clone, Copy, Debug)]
    enum Wire {
        #[cfg(feature = "openai")]
        Chat,
        #[cfg(feature = "openai")]
        Azure,
        #[cfg(feature = "deepseek")]
        DeepSeek,
        #[cfg(feature = "gemini")]
        Gemini,
    }

    fn wires() -> Vec<Wire> {
        vec![
            #[cfg(feature = "openai")]
            Wire::Chat,
            #[cfg(feature = "openai")]
            Wire::Azure,
            #[cfg(feature = "deepseek")]
            Wire::DeepSeek,
            #[cfg(feature = "gemini")]
            Wire::Gemini,
        ]
    }

    impl Wire {
        fn model(self, base: String) -> Box<dyn Llm> {
            use adk_model::retry::RetryConfig;
            match self {
                #[cfg(feature = "openai")]
                Self::Chat => Box::new(
                    adk_model::openai::OpenAIClient::compatible("key", base, "scripted")
                        .unwrap()
                        .with_retry_config(RetryConfig::disabled()),
                ),
                #[cfg(feature = "openai")]
                Self::Azure => Box::new(
                    adk_model::openai::AzureOpenAIClient::new(adk_model::openai::AzureConfig::new(
                        "key",
                        base,
                        "2024-12-01-preview",
                        "scripted",
                    ))
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
                ),
                #[cfg(feature = "deepseek")]
                Self::DeepSeek => Box::new(
                    adk_model::DeepSeekClient::new(
                        adk_model::DeepSeekConfig::new("key", "scripted").with_base_url(base),
                    )
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
                ),
                #[cfg(feature = "gemini")]
                Self::Gemini => Box::new(
                    adk_model::GeminiModel::new_with_base_url(
                        "key",
                        "scripted",
                        format!("{base}/v1beta/"),
                    )
                    .unwrap()
                    .with_retry_config(RetryConfig::disabled()),
                ),
            }
        }

        fn gemini(self) -> bool {
            match self {
                #[cfg(feature = "gemini")]
                Self::Gemini => true,
                #[cfg(feature = "openai")]
                Self::Chat | Self::Azure => false,
                #[cfg(feature = "deepseek")]
                Self::DeepSeek => false,
            }
        }

        fn finished(self, tool: bool) -> Vec<Vec<u8>> {
            if self.gemini() {
                let parts = if tool {
                    json!([{"functionCall": {"id": "call_1", "name": "get_weather", "args": {"city": "Paris"}}}])
                } else {
                    json!([{"text": "Paris."}])
                };
                return chat::sse(&[
                    json!({"candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": "STOP"}]}),
                ]);
            }
            if tool {
                let mut chunks = chat::tool_call_chunks(&[r#"{"city":"Paris"}"#]);
                chunks.push(chat::chunk(json!({}), Some("tool_calls")));
                chat::sse(&chunks)
            } else {
                let mut writes = chat::finished_text(&["Paris."]);
                writes.pop();
                writes
            }
        }

        fn usage(self) -> Vec<u8> {
            let value = if self.gemini() {
                json!({"candidates": [], "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3, "totalTokenCount": 10}})
            } else {
                let mut value = chat::chunk(json!({}), None);
                value["choices"] = json!([]);
                value["usage"] =
                    json!({"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10});
                value
            };
            format!("data: {value}\n\n").into_bytes()
        }
    }

    #[tokio::test]
    async fn combines_trailing_usage_without_repeating_tools() {
        for wire in wires() {
            for tool in [false, true] {
                let mut writes = wire.finished(tool);
                writes.push(wire.usage());
                if !wire.gemini() {
                    writes.push(chat::done());
                }
                let (base, calls) =
                    serve_tail("text/event-stream", writes, Some(b"0\r\n\r\n")).await;
                let (responses, error) =
                    collect(wire.model(base).as_ref(), request("scripted"), true).await;
                assert!(error.is_none(), "{wire:?}: {error:?}");
                assert_eq!(calls.load(Ordering::SeqCst), 1, "{wire:?}");
                let finals: Vec<_> =
                    responses.iter().filter(|response| !response.partial).collect();
                assert_eq!(finals.len(), 1, "{wire:?}: {responses:?}");
                let usage = finals[0].usage_metadata.as_ref().expect("trailing usage is retained");
                assert_eq!(
                    (
                        usage.prompt_token_count,
                        usage.candidates_token_count,
                        usage.total_token_count
                    ),
                    (7, 3, 10),
                    "{wire:?}"
                );
                assert_eq!(function_calls(&responses), usize::from(tool), "{wire:?}");
                if !tool {
                    let text: String = responses
                        .iter()
                        .filter_map(|r| r.content.as_ref())
                        .flat_map(|c| &c.parts)
                        .filter_map(Part::text)
                        .collect();
                    assert_eq!(text, "Paris.", "{wire:?}");
                }
            }
        }
    }

    #[tokio::test]
    async fn completed_answers_survive_transport_tail_failures() {
        for wire in wires() {
            let (base, calls) =
                serve_tail("text/event-stream", wire.finished(false), Some(b"")).await;
            let (responses, error) =
                collect(wire.model(base).as_ref(), request("scripted"), true).await;
            assert!(error.is_none(), "{wire:?}: {error:?}");
            assert_eq!(responses.iter().filter(|r| !r.partial).count(), 1, "{wire:?}");
            assert!(
                responses.last().unwrap().usage_metadata.is_none(),
                "missing usage stays unreported"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{wire:?}");
        }
    }

    #[tokio::test]
    async fn reported_tail_failures_preserve_completion() {
        for wire in wires() {
            let mut writes = wire.finished(false);
            let failure = if wire.gemini() {
                json!({"error": {"code": 503, "status": "UNAVAILABLE", "message": "failed"}})
            } else {
                json!({"error": {"code": "server_error", "message": "failed"}})
            };
            writes.extend(chat::sse(&[failure]));
            let base = serve("text/event-stream", writes).await;
            let (responses, error) =
                collect(wire.model(base).as_ref(), request("scripted"), true).await;
            assert!(
                error.is_none(),
                "the runner's completed answer is authoritative: {wire:?}: {error:?}"
            );
            assert_eq!(
                responses.iter().filter(|response| !response.partial).count(),
                1,
                "{wire:?}"
            );
            assert!(
                responses.last().unwrap().usage_metadata.is_none(),
                "a failed usage trailer is unreported: {wire:?}"
            );
        }
    }

    #[cfg(feature = "openai")]
    #[tokio::test]
    async fn retains_buffered_literal_prefixes() {
        let mut writes =
            chat::sse(&[chat::text_chunk("<tool"), chat::chunk(json!({}), Some("stop"))]);
        writes.push(Wire::Chat.usage());
        writes.push(chat::done());
        let base = serve("text/event-stream", writes).await;
        let mut input = request("scripted");
        input.tools.insert("get_weather".into(), json!({"type": "object", "properties": {}}));
        let (responses, error) = collect(Wire::Chat.model(base).as_ref(), input, true).await;
        assert!(error.is_none(), "{error:?}");
        let text: String = responses
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(Part::text)
            .collect();
        assert_eq!(text, "<tool");
        assert_eq!(function_calls(&responses), 0);
    }

    #[tokio::test]
    async fn partial_usage_is_not_final_usage() {
        for wire in wires() {
            let mut writes = vec![wire.usage()];
            writes.extend(wire.finished(false));
            let base = serve("text/event-stream", writes).await;
            let (responses, error) =
                collect(wire.model(base).as_ref(), request("scripted"), true).await;
            assert!(error.is_none(), "{wire:?}: {error:?}");
            assert!(
                responses
                    .iter()
                    .any(|response| response.partial && response.usage_metadata.is_some()),
                "{wire:?}"
            );
            let final_response = responses.iter().find(|response| !response.partial).unwrap();
            assert!(
                final_response.usage_metadata.is_none(),
                "partial prompt/cache counts are not a final report: {wire:?}"
            );
        }
    }

    #[tokio::test]
    async fn completion_bounds_socket_tail_wait() {
        for wire in wires() {
            for reported in [false, true] {
                let mut writes = wire.finished(false);
                if reported {
                    writes.push(wire.usage());
                }
                if !wire.gemini() {
                    writes.push(chat::done());
                }
                let (base, calls) = serve_tail("text/event-stream", writes, None).await;
                let started = std::time::Instant::now();
                let (responses, error) = tokio::time::timeout(
                    Duration::from_secs(7),
                    collect(wire.model(base).as_ref(), request("scripted"), true),
                )
                .await
                .expect("tail grace must be bounded");
                assert!(error.is_none(), "{wire:?}: {error:?}");
                assert_eq!(responses.iter().filter(|r| !r.partial).count(), 1, "{wire:?}");
                let usage = responses.last().unwrap().usage_metadata.as_ref();
                assert_eq!(
                    usage.map(|usage| usage.total_token_count),
                    reported.then_some(10),
                    "{wire:?}"
                );
                if reported || !wire.gemini() {
                    assert!(
                        started.elapsed() < Duration::from_secs(2),
                        "usage or DONE must finish without waiting for socket EOF: {wire:?}"
                    );
                }
                assert_eq!(calls.load(Ordering::SeqCst), 1, "{wire:?}");
            }
        }
    }
}
