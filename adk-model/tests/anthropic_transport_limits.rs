#![cfg(feature = "anthropic")]

use adk_core::{Content, ErrorCategory, Llm, LlmRequest};
use adk_model::{
    HttpTimeouts,
    anthropic::{AnthropicClient, AnthropicConfig},
    retry::RetryConfig,
};
use futures::StreamExt;
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn request() -> LlmRequest {
    LlmRequest::new("claude-sonnet-4-6", vec![Content::new("user").with_text("Hello")])
}

#[tokio::test]
async fn preserves_http_status_and_retry_delay() {
    for (status, category) in [
        (429, ErrorCategory::RateLimited),
        (500, ErrorCategory::Unavailable),
        (502, ErrorCategory::Unavailable),
        (503, ErrorCategory::Unavailable),
        (504, ErrorCategory::Unavailable),
        (529, ErrorCategory::Unavailable),
        (599, ErrorCategory::Internal),
    ] {
        for stream in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/v1/messages"))
                .respond_with(ResponseTemplate::new(status).insert_header("retry-after","17").set_body_json(serde_json::json!({"error":{"type":"api_error","message":"Fixture failure"}})))
                .expect(1).mount(&server).await;
            let model = AnthropicClient::new(
                AnthropicConfig::new("fixture-key", "claude-sonnet-4-6")
                    .with_base_url(server.uri()),
            )
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
            let result = model.generate_content(request(), stream).await;
            let error = match result {
                Err(error) => error,
                Ok(mut events) => events.next().await.unwrap().unwrap_err(),
            };
            assert_eq!(error.details.upstream_status_code, Some(status));
            assert_eq!(error.category, category);
            assert_eq!(
                error.retry.retry_after(),
                error.retry.should_retry.then_some(Duration::from_secs(17))
            );
            server.verify().await;
        }
    }
}

#[tokio::test]
async fn classifies_response_header_timeouts() {
    for stream in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(300)))
            .expect(1)
            .mount(&server)
            .await;
        let model = AnthropicClient::new(
            AnthropicConfig::new("fixture-key", "claude-sonnet-4-6").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled())
        .with_timeouts(HttpTimeouts::new(Duration::from_secs(1), Duration::from_millis(40)))
        .unwrap();
        let error = match model.generate_content(request(), stream).await {
            Err(error) => error,
            Ok(mut events) => events.next().await.unwrap().unwrap_err(),
        };
        assert_eq!(error.category, ErrorCategory::Timeout);
        assert_eq!(error.code, "model.anthropic.timeout");
        assert!(error.retry.should_retry);
        server.verify().await;
    }
}
