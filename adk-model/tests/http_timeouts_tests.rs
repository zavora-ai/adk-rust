//! A request that gets no response headers, a stream that stops sending data, and a complete
//! response whose body stalls fail after the client's idle timeout with a `Timeout` error
//! instead of holding the caller.
#![cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "groq",
    feature = "ollama",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "openrouter",
    feature = "bedrock"
))]

use std::time::{Duration, Instant};

use adk_core::{Content, ErrorCategory, Llm, LlmRequest};
use adk_model::HttpTimeouts;
use futures::StreamExt;
use tokio::io::AsyncReadExt;
#[cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "ollama",
    feature = "openrouter"
))]
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Answers every request with response headers and `first` as one chunk, then sends nothing.
#[cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "ollama",
    feature = "openrouter"
))]
async fn stalling(content_type: &'static str, first: &'static str) -> String {
    chunked(content_type, vec![first], Duration::ZERO, false).await
}

async fn chunked(
    content_type: &'static str,
    chunks: Vec<&'static str>,
    gap: Duration,
    complete: bool,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let chunks = std::sync::Arc::new(chunks);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let chunks = chunks.clone();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 8192];
                let _ = socket.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n"
                );
                let _ = socket.write_all(head.as_bytes()).await;
                for chunk in chunks.iter() {
                    let frame = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                    if socket.write_all(frame.as_bytes()).await.is_err()
                        || socket.flush().await.is_err()
                    {
                        return;
                    }
                    tokio::time::sleep(gap).await;
                }
                if complete {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                } else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
        }
    });
    base
}

/// Accepts every connection and reads the request, but never sends a response.
async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 8192];
                let _ = socket.read(&mut buffer).await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });
    base
}

fn timeouts() -> HttpTimeouts {
    HttpTimeouts::new(Duration::from_secs(2), Duration::from_millis(300))
}

/// Streams one turn and returns the error that ended it, within a bound far above the idle
/// timeout.
#[cfg(any(feature = "openai", feature = "groq", feature = "ollama", feature = "openrouter"))]
async fn stalled_error(model: &dyn Llm, model_id: &str) -> adk_core::AdkError {
    stalled_call(model, model_id, true).await
}

/// Runs one turn, streamed or not, and returns the error that ended it.
async fn stalled_call(model: &dyn Llm, model_id: &str, stream: bool) -> adk_core::AdkError {
    let request = LlmRequest::new(model_id, vec![Content::new("user").with_text("Hello")]);
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = model.generate_content(request, stream).await?;
        while let Some(item) = stream.next().await {
            item?;
        }
        Ok::<(), adk_core::AdkError>(())
    })
    .await
    .expect("the stalled call must fail within the idle timeout, not hang");
    let error = outcome.expect_err("a stalled call fails");
    assert!(started.elapsed() >= Duration::from_millis(300), "failed before the idle timeout");
    error
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_compatible_stalled_stream_times_out() {
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
    let first = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";
    for headers_first in [true, false] {
        let base = stalling("text/event-stream", first).await;
        let mut headers = http::HeaderMap::new();
        headers.insert("x-session", "fixture".parse().unwrap());
        let model =
            OpenAICompatible::new(OpenAICompatibleConfig::new("key", "m").with_base_url(base))
                .unwrap()
                .with_retry_config(adk_model::RetryConfig::disabled());
        let model = if headers_first {
            model.with_default_headers(headers).unwrap().with_timeouts(timeouts()).unwrap()
        } else {
            model.with_timeouts(timeouts()).unwrap().with_default_headers(headers).unwrap()
        };
        let error = stalled_error(&model, "m").await;
        assert_eq!(error.category, ErrorCategory::Timeout, "{error}");
        assert_eq!(error.code, "model.openai_compat.incomplete_stream");
    }
}

#[cfg(feature = "groq")]
#[tokio::test]
async fn groq_stalled_stream_times_out() {
    use adk_model::groq::{GroqClient, GroqConfig};
    let first = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";
    let base = stalling("text/event-stream", first).await;
    let model = GroqClient::new(GroqConfig::new("key", "m").with_base_url(base))
        .unwrap()
        .with_timeouts(timeouts())
        .unwrap()
        .with_retry_config(adk_model::RetryConfig::disabled());
    let error = stalled_error(&model, "m").await;
    assert_eq!(error.category, ErrorCategory::Timeout, "{error}");
}

#[cfg(feature = "ollama")]
#[tokio::test]
async fn ollama_stalled_stream_times_out() {
    use adk_model::ollama::{OllamaConfig, OllamaModel};
    let first = "{\"model\":\"m\",\"created_at\":\"2026-10-18T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"done\":false}\n";
    let base = stalling("application/x-ndjson", first).await;
    let model = OllamaModel::new(OllamaConfig::with_host(base, "m"))
        .unwrap()
        .with_timeouts(timeouts())
        .unwrap();
    let error = stalled_error(&model, "m").await;
    assert_eq!(error.category, ErrorCategory::Timeout, "{error}");
}

/// A chat completion's first chunk.
#[cfg(feature = "openrouter")]
const CHAT_CHUNK: &str = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";

/// A Responses stream's first event.
#[cfg(any(feature = "openai", feature = "openrouter"))]
const RESPONSE_CREATED: &str = "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"model\":\"m\",\"output\":[]}}\n\n";

/// Asserts that a call against no response, and a complete response whose body stalls, both
/// time out.
// Gated on the providers whose tests call it; Gemini's stall tests use their own helpers.
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "ollama",
    feature = "openrouter"
))]
async fn headers_and_body_time_out<M: Llm>(build: impl Fn(String) -> M) {
    let error = stalled_call(&build(silent().await), "m", true).await;
    assert_eq!(error.category, ErrorCategory::Timeout, "no response headers: {error}");
    let error = stalled_call(&build(stalling("application/json", "{").await), "m", false).await;
    assert_eq!(error.category, ErrorCategory::Timeout, "stalled body: {error}");
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_compatible_stalled_request_and_body_time_out() {
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
    headers_and_body_time_out(|base| {
        OpenAICompatible::new(OpenAICompatibleConfig::new("key", "m").with_base_url(base))
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
    })
    .await;
}

#[cfg(feature = "groq")]
#[tokio::test]
async fn groq_stalled_request_and_body_time_out() {
    use adk_model::groq::{GroqClient, GroqConfig};
    headers_and_body_time_out(|base| {
        GroqClient::new(GroqConfig::new("key", "m").with_base_url(base))
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
    })
    .await;
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn deepseek_stalled_request_and_body_time_out() {
    use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
    headers_and_body_time_out(|base| {
        DeepSeekClient::new(DeepSeekConfig::new("key", "m").with_base_url(base))
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
    })
    .await;
}

#[cfg(feature = "azure-ai")]
#[tokio::test]
async fn azure_ai_stalled_request_and_body_time_out() {
    use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
    headers_and_body_time_out(|base| {
        AzureAIClient::new(AzureAIConfig::new(base, "key", "m"))
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
    })
    .await;
}

#[cfg(feature = "ollama")]
#[tokio::test]
async fn ollama_stalled_request_and_body_time_out() {
    use adk_model::ollama::{OllamaConfig, OllamaModel};
    headers_and_body_time_out(|base| {
        OllamaModel::new(OllamaConfig::with_host(base, "m"))
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
    })
    .await;
}

#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_stalled_requests_bodies_and_streams_time_out() {
    use adk_model::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
    for (mode, first) in [
        (OpenRouterApiMode::ChatCompletions, CHAT_CHUNK),
        (OpenRouterApiMode::Responses, RESPONSE_CREATED),
    ] {
        let build = |base: String| {
            OpenRouterClient::new(
                OpenRouterConfig::new("key", "m").with_base_url(base).with_default_api_mode(mode),
            )
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
        };
        headers_and_body_time_out(build).await;
        let error = stalled_error(&build(stalling("text/event-stream", first).await), "m").await;
        assert_eq!(error.category, ErrorCategory::Timeout, "{mode:?} stream: {error}");
    }
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_responses_stalled_request_body_and_stream_time_out() {
    use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
    for open_responses in [false, true] {
        let build = |base: String| {
            OpenAIResponsesClient::new(
                OpenAIResponsesConfig::new("key", "m")
                    .with_base_url(base)
                    .with_open_responses_mode(open_responses),
            )
            .unwrap()
            .with_timeouts(timeouts())
            .unwrap()
            .with_retry_config(adk_model::RetryConfig::disabled())
        };
        headers_and_body_time_out(build).await;
        let error =
            stalled_error(&build(stalling("text/event-stream", RESPONSE_CREATED).await), "m").await;
        assert_eq!(error.category, ErrorCategory::Timeout, "stream: {error}");
        assert_eq!(error.code, "model.openai_responses.stream");
    }
}

#[cfg(feature = "bedrock")]
#[tokio::test]
async fn bedrock_request_without_response_times_out() {
    use adk_model::bedrock::{BedrockClient, BedrockConfig};
    let base = silent().await;
    let model = BedrockClient::new_with_api_key(
        BedrockConfig::new("us-east-1", "m").with_endpoint_url(base),
        "key",
    )
    .unwrap()
    .with_timeouts(timeouts())
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());
    for stream in [true, false] {
        let error = stalled_call(&model, "m", stream).await;
        assert_eq!(error.category, ErrorCategory::Timeout, "stream {stream}: {error}");
        assert_eq!(error.code, "model.bedrock.timeout");
    }
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_stalled_headers_bodies_and_streams_time_out() {
    use adk_model::GeminiModel;
    let cases = [
        (silent().await, true),
        (stalling("application/json", "{").await, false),
        (stalling("text/event-stream", ": waiting\n\n").await, true),
    ];
    for (base, stream) in cases {
        let model = GeminiModel::new_with_base_url_and_timeouts(
            "key",
            "m",
            format!("{base}/v1beta/"),
            timeouts(),
        )
        .unwrap()
        .with_retry_config(adk_model::retry::RetryConfig::disabled());
        let error = stalled_call(&model, "m", stream).await;
        assert_eq!(error.category, ErrorCategory::Timeout, "{error:?}");
        assert!(error.is_retryable());
        assert_eq!(error.details.upstream_status_code, None);
    }
    let base = chunked("application/json", vec!["not-json"], Duration::ZERO, true).await;
    let model = adk_model::GeminiModel::new_with_base_url_and_timeouts(
        "key",
        "m",
        format!("{base}/v1beta/"),
        timeouts(),
    )
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());
    let request = LlmRequest::new("m", vec![Content::new("user").with_text("Hello")]);
    let error = match model.generate_content(request, false).await {
        Err(error) => error,
        Ok(mut stream) => stream.next().await.unwrap().unwrap_err(),
    };
    assert_eq!(error.category, ErrorCategory::Internal);
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_comments_reset_byte_idle() {
    use adk_model::GeminiModel;
    let mut chunks = vec![": heartbeat\n\n"; 6];
    chunks.push("data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}]}\n\n");
    let base = chunked("text/event-stream", chunks, Duration::from_millis(75), true).await;
    let model = GeminiModel::new_with_base_url_and_timeouts(
        "key",
        "m",
        format!("{base}/v1beta/"),
        timeouts(),
    )
    .unwrap()
    .with_retry_config(adk_model::retry::RetryConfig::disabled());
    let started = Instant::now();
    let mut responses = model
        .generate_content(LlmRequest::new("m", vec![Content::new("user").with_text("Hello")]), true)
        .await
        .unwrap();
    let mut finished = false;
    while let Some(response) = responses.next().await {
        finished |= response.unwrap().turn_complete;
    }
    assert!(finished);
    assert!(
        started.elapsed() > timeouts().idle,
        "no model chunk arrived for more than the byte-idle window"
    );
}

#[cfg(feature = "opencode")]
#[tokio::test]
async fn opencode_forwards_timeouts_to_every_protocol() {
    use adk_model::{OpenCodeApi, OpenCodeClient, OpenCodeConfig, OpenCodeService};
    for api in [
        OpenCodeApi::ChatCompletions,
        OpenCodeApi::Responses,
        OpenCodeApi::Messages,
        OpenCodeApi::GenerateContent,
    ] {
        headers_and_body_time_out(|base| {
            OpenCodeClient::new(
                OpenCodeConfig::new(OpenCodeService::Go, "key", "m")
                    .with_api(api)
                    .with_base_url(format!("{base}/v1"))
                    .with_user_agent("adk-test/1.0")
                    .with_session_id("fixture")
                    .with_timeouts(timeouts())
                    .with_retry_config(adk_model::RetryConfig::disabled()),
            )
            .unwrap()
        })
        .await;
    }
}
