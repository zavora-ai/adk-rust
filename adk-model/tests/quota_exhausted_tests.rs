//! An account out of quota or credit is `ResourceExhausted` and is not retried, while a
//! rate limit on the same HTTP status still is.
#![cfg(any(feature = "openai", feature = "deepseek", feature = "anthropic"))]

use std::time::Duration;

use adk_core::{AdkError, Content, ErrorCategory, Llm, LlmRequest};
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// OpenAI's body for an account with no credit, live on 2026-11-16.
#[cfg(feature = "openai")]
fn no_credit() -> Value {
    json!({"error": {
        "message": "You have no credits remaining. Add credits to continue using the API at https://platform.openai.com/settings/organization/billing/.",
        "type": "insufficient_quota",
        "param": null,
        "code": "credit_balance_exhausted"
    }})
}

fn retrying() -> RetryConfig {
    RetryConfig::default()
        .with_max_retries(2)
        .with_initial_delay(Duration::ZERO)
        .with_max_delay(Duration::ZERO)
}

async fn first_error(model: &dyn Llm, stream: bool) -> AdkError {
    let request = LlmRequest::new("model", vec![Content::new("user").with_text("Hello")]);
    match model.generate_content(request, stream).await {
        Err(error) => error,
        Ok(mut events) => events.next().await.expect("stream yields an item").expect_err("fails"),
    }
}

/// Serves `status` with `body` on `route` and returns the error and the number of requests.
async fn fail_with(
    route: &str,
    status: u16,
    body: Value,
    build: impl Fn(&str) -> Box<dyn Llm>,
    stream: bool,
) -> (AdkError, usize) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    let model = build(&server.uri());
    let error = first_error(model.as_ref(), stream).await;
    (error, server.received_requests().await.unwrap().len())
}

fn assert_exhausted(error: &AdkError, code: &str, status: u16) {
    assert_eq!(
        (error.category, error.code, error.is_retryable()),
        (ErrorCategory::ResourceExhausted, code, false),
        "{error:?}"
    );
    assert_eq!(error.details.upstream_status_code, Some(status));
    assert!(error.message.contains("add credit or raise the limit"), "{}", error.message);
}

#[cfg(feature = "openai")]
mod openai {
    use super::*;
    use adk_model::openai::{
        AzureConfig, AzureOpenAIClient, OpenAIResponsesClient, OpenAIResponsesConfig,
    };
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

    fn compatible(uri: &str) -> Box<dyn Llm> {
        Box::new(
            OpenAICompatible::new(OpenAICompatibleConfig::new("key", "model").with_base_url(uri))
                .unwrap()
                .with_retry_config(retrying()),
        )
    }

    #[tokio::test]
    async fn chat_completions_reports_no_credit_once() {
        for stream in [false, true] {
            let (error, requests) =
                fail_with("chat/completions$", 429, no_credit(), compatible, stream).await;
            assert_exhausted(&error, "model.openai_compat.quota_exhausted", 429);
            assert_eq!(error.details.metadata["provider_error_code"], "credit_balance_exhausted");
            assert_eq!(requests, 1);
        }
    }

    #[tokio::test]
    async fn chat_completions_still_retries_a_rate_limit() {
        let rate_limited = json!({"error": {
            "message": "Rate limit reached",
            "type": "requests",
            "code": "rate_limit_exceeded"
        }});
        let (error, requests) =
            fail_with("chat/completions$", 429, rate_limited, compatible, false).await;
        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.code, "model.openai_compat.api_error");
        assert_eq!(requests, 3);
    }

    #[tokio::test]
    async fn azure_openai_reports_no_credit_once() {
        let azure = |uri: &str| -> Box<dyn Llm> {
            Box::new(
                AzureOpenAIClient::new(AzureConfig::new("key", uri, "2024-12-01-preview", "model"))
                    .unwrap()
                    .with_retry_config(retrying()),
            )
        };
        let (error, requests) =
            fail_with("chat/completions$", 429, no_credit(), azure, false).await;
        assert_exhausted(&error, "model.azure_openai.quota_exhausted", 429);
        assert_eq!(requests, 1);
    }

    #[tokio::test]
    async fn responses_reports_no_credit_once() {
        let responses = |uri: &str| -> Box<dyn Llm> {
            Box::new(
                OpenAIResponsesClient::new(
                    OpenAIResponsesConfig::new("key", "model").with_base_url(uri),
                )
                .unwrap()
                .with_retry_config(retrying()),
            )
        };
        for stream in [false, true] {
            let (error, requests) =
                fail_with("responses$", 429, no_credit(), responses, stream).await;
            assert_exhausted(&error, "model.openai_responses.quota_exhausted", 429);
            assert_eq!(requests, 1);
        }
    }
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn deepseek_insufficient_balance_is_not_retried() {
    use adk_model::{DeepSeekClient, DeepSeekConfig};

    let deepseek = |uri: &str| -> Box<dyn Llm> {
        Box::new(
            DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(uri))
                .unwrap()
                .with_retry_config(retrying()),
        )
    };
    let body = json!({"error": {
        "message": "Insufficient Balance",
        "type": "unknown_error",
        "param": null,
        "code": "invalid_request_error"
    }});
    let (error, requests) = fail_with("chat/completions$", 402, body, deepseek, false).await;
    assert_exhausted(&error, "model.deepseek.api_error", 402);
    assert_eq!(requests, 1);
}

#[cfg(feature = "anthropic")]
#[tokio::test]
async fn anthropic_billing_error_is_not_retried() {
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

    let anthropic = |uri: &str| -> Box<dyn Llm> {
        Box::new(
            AnthropicClient::new(
                AnthropicConfig::new("key", "claude-sonnet-5-5").with_base_url(uri),
            )
            .unwrap()
            .with_retry_config(retrying()),
        )
    };
    let body = json!({"type": "error", "error": {
        "type": "billing_error",
        "message": "Your credit balance is too low to access the Anthropic API."
    }});
    for stream in [false, true] {
        let (error, requests) =
            fail_with("v1/messages$", 402, body.clone(), anthropic, stream).await;
        assert_exhausted(&error, "model.anthropic.api_error", 402);
        assert_eq!(requests, 1);
    }
}
