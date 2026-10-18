//! A stream that stops sending data fails after the client's idle timeout with a `Timeout`
//! error instead of holding the caller.
#![cfg(any(feature = "openai", feature = "groq", feature = "ollama"))]

use std::time::{Duration, Instant};

use adk_core::{Content, ErrorCategory, Llm, LlmRequest};
use adk_model::HttpTimeouts;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Answers every request with response headers and `first` as one chunk, then sends nothing.
async fn stalling(content_type: &'static str, first: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 8192];
                let _ = socket.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n",
                    first.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.flush().await;
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
async fn stalled_error(model: &dyn Llm, model_id: &str) -> adk_core::AdkError {
    let request = LlmRequest::new(model_id, vec![Content::new("user").with_text("Hello")]);
    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = model.generate_content(request, true).await?;
        while let Some(item) = stream.next().await {
            item?;
        }
        Ok::<(), adk_core::AdkError>(())
    })
    .await
    .expect("the stalled stream must fail within the idle timeout, not hang");
    let error = outcome.expect_err("a stalled stream fails");
    assert!(started.elapsed() >= Duration::from_millis(300), "failed before the idle timeout");
    error
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_compatible_stalled_stream_times_out() {
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
    let first = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n";
    let base = stalling("text/event-stream", first).await;
    let model = OpenAICompatible::new(OpenAICompatibleConfig::new("key", "m").with_base_url(base))
        .unwrap()
        .with_timeouts(timeouts())
        .unwrap()
        .with_retry_config(adk_model::RetryConfig::disabled());
    let error = stalled_error(&model, "m").await;
    assert_eq!(error.category, ErrorCategory::Timeout, "{error}");
    assert_eq!(error.code, "model.openai_compat.incomplete_stream");
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
