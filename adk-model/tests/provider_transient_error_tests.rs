//! Transient upstream failures (HTTP 500/502/503/504) are retryable for every
//! HTTP provider client, carry the upstream status, and preserve `Retry-After`
//! without shortening a server delay.
#![cfg(any(feature = "openai", feature = "groq", feature = "deepseek", feature = "azure-ai"))]

use std::time::Duration;

use adk_core::{AdkError, Content, ErrorCategory, Llm, LlmRequest};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> LlmRequest {
    LlmRequest::new("model", vec![Content::new("user").with_text("Hello")])
}

/// Retries once with bounded exponential backoff. Server hints retain their own limit.
fn fast_retry() -> RetryConfig {
    RetryConfig::default()
        .with_max_retries(1)
        .with_initial_delay(Duration::ZERO)
        .with_max_delay(Duration::from_millis(50))
}

async fn first_error(model: &dyn Llm) -> AdkError {
    let mut stream = model.generate_content(request(), false).await.expect("stream starts");
    stream.next().await.expect("stream yields an item").expect_err("request fails")
}

/// With retries disabled, a 502 maps to a retryable error carrying the status
/// and the `Retry-After` delay.
async fn assert_502_is_retryable(server: &MockServer, model: &dyn Llm) {
    Mock::given(method("POST"))
        .and(path_regex("chat/completions$"))
        .respond_with(
            ResponseTemplate::new(502).insert_header("retry-after", "7").set_body_string("bad"),
        )
        .mount(server)
        .await;

    let error = first_error(model).await;

    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert!(error.is_retryable());
    assert_eq!(error.details.upstream_status_code, Some(502));
    assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(7)));
}

/// A one-hour server delay is refused without issuing a second request.
async fn assert_503_delay_is_refused(server: &MockServer, model: &dyn Llm) {
    Mock::given(method("POST"))
        .and(path_regex("chat/completions$"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "3600"))
        .expect(1)
        .mount(server)
        .await;
    let started = std::time::Instant::now();
    let error = first_error(model).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "server delay should be refused immediately"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(error.category, ErrorCategory::Unavailable);
    assert_eq!(error.details.upstream_status_code, Some(503));
    assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(3600)));
}

#[cfg(feature = "openai")]
mod openai {
    use super::*;
    use adk_model::openai::{AzureConfig, AzureOpenAIClient};
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

    fn compatible(uri: &str, retry: RetryConfig) -> OpenAICompatible {
        OpenAICompatible::new(OpenAICompatibleConfig::new("key", "model").with_base_url(uri))
            .unwrap()
            .with_retry_config(retry)
    }

    fn azure(uri: &str, retry: RetryConfig) -> AzureOpenAIClient {
        AzureOpenAIClient::new(AzureConfig::new("key", uri, "2024-12-01-preview", "model"))
            .unwrap()
            .with_retry_config(retry)
    }

    #[tokio::test]
    async fn compatible_502_is_retryable() {
        let server = MockServer::start().await;
        assert_502_is_retryable(&server, &compatible(&server.uri(), RetryConfig::disabled())).await;
    }

    #[tokio::test]
    async fn compatible_503_delay_is_refused() {
        let server = MockServer::start().await;
        assert_503_delay_is_refused(&server, &compatible(&server.uri(), fast_retry())).await;
    }

    #[tokio::test]
    async fn azure_openai_502_is_retryable() {
        let server = MockServer::start().await;
        assert_502_is_retryable(&server, &azure(&server.uri(), RetryConfig::disabled())).await;
    }

    #[tokio::test]
    async fn azure_openai_503_delay_is_refused() {
        let server = MockServer::start().await;
        assert_503_delay_is_refused(&server, &azure(&server.uri(), fast_retry())).await;
    }
}

#[cfg(feature = "groq")]
mod groq {
    use super::*;
    use adk_model::{GroqClient, GroqConfig};

    fn client(uri: &str, retry: RetryConfig) -> GroqClient {
        GroqClient::new(GroqConfig::new("key", "model").with_base_url(uri))
            .unwrap()
            .with_retry_config(retry)
    }

    #[tokio::test]
    async fn groq_502_is_retryable() {
        let server = MockServer::start().await;
        assert_502_is_retryable(&server, &client(&server.uri(), RetryConfig::disabled())).await;
    }

    #[tokio::test]
    async fn groq_503_delay_is_refused() {
        let server = MockServer::start().await;
        assert_503_delay_is_refused(&server, &client(&server.uri(), fast_retry())).await;
    }
}

#[cfg(feature = "deepseek")]
mod deepseek {
    use super::*;
    use adk_model::{DeepSeekClient, DeepSeekConfig};

    fn client(uri: &str, retry: RetryConfig) -> DeepSeekClient {
        DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(uri))
            .unwrap()
            .with_retry_config(retry)
    }

    #[tokio::test]
    async fn deepseek_502_is_retryable() {
        let server = MockServer::start().await;
        assert_502_is_retryable(&server, &client(&server.uri(), RetryConfig::disabled())).await;
    }

    #[tokio::test]
    async fn deepseek_503_delay_is_refused() {
        let server = MockServer::start().await;
        assert_503_delay_is_refused(&server, &client(&server.uri(), fast_retry())).await;
    }
}

#[cfg(feature = "azure-ai")]
mod azure_ai {
    use super::*;
    use adk_model::{AzureAIClient, AzureAIConfig};

    fn client(uri: &str, retry: RetryConfig) -> AzureAIClient {
        AzureAIClient::new(AzureAIConfig::new(uri, "key", "model"))
            .unwrap()
            .with_retry_config(retry)
    }

    #[tokio::test]
    async fn azure_ai_502_is_retryable() {
        let server = MockServer::start().await;
        assert_502_is_retryable(&server, &client(&server.uri(), RetryConfig::disabled())).await;
    }

    #[tokio::test]
    async fn azure_ai_503_delay_is_refused() {
        let server = MockServer::start().await;
        assert_503_delay_is_refused(&server, &client(&server.uri(), fast_retry())).await;
    }
}
