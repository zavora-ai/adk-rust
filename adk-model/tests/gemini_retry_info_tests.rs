//! A Gemini 429 that names its wait in a `google.rpc.RetryInfo` detail is retried after that
//! wait, bounded by the retry policy, not after the short exponential backoff.

#![cfg(feature = "gemini")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{Content, ErrorCategory, Llm, LlmRequest, Part};
use adk_model::GeminiModel;
use adk_model::retry::RetryConfig;
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "gemini-3.7-flash";

/// The body the Gemini API sends when a quota is exhausted.
fn quota_exhausted(retry_delay: &str) -> ResponseTemplate {
    ResponseTemplate::new(429).set_body_json(json!({"error": {
        "code": 429,
        "message": "You exceeded your current quota, please check your plan and billing details.",
        "status": "RESOURCE_EXHAUSTED",
        "details": [
            {
                "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                "violations": [{"quotaMetric": "generativelanguage.googleapis.com/generate_content_free_tier_requests"}]
            },
            {"@type": "type.googleapis.com/google.rpc.Help", "links": []},
            {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": retry_delay}
        ]
    }}))
}

fn answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP",
            "index": 0
        }],
        "modelVersion": MODEL
    }))
}

fn model(server: &MockServer, retry: RetryConfig) -> GeminiModel {
    GeminiModel::new_with_base_url("test-key", MODEL, format!("{}/v1beta/", server.uri()))
        .unwrap()
        .with_retry_config(retry)
}

fn request() -> LlmRequest {
    LlmRequest::new(MODEL, vec![Content::new("user").with_text("Hello")])
}

/// Answers the first request with a quota error naming `retry_delay` and every later one with
/// an answer, recording when each request arrived.
struct QuotaThenAnswer {
    retry_delay: &'static str,
    arrivals: Arc<Mutex<Vec<Instant>>>,
}

impl Respond for QuotaThenAnswer {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut arrivals = self.arrivals.lock().unwrap();
        arrivals.push(Instant::now());
        if arrivals.len() == 1 { quota_exhausted(self.retry_delay) } else { answer() }
    }
}

#[tokio::test]
async fn waits_the_retry_delay_before_retrying() {
    let server = MockServer::start().await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(QuotaThenAnswer { retry_delay: "0.4s", arrivals: arrivals.clone() })
        .expect(2)
        .mount(&server)
        .await;
    // Backoff alone would retry at once.
    let retry = RetryConfig::default()
        .with_max_retries(1)
        .with_initial_delay(Duration::ZERO)
        .with_max_delay(Duration::ZERO);

    let mut responses = model(&server, retry).generate_content(request(), false).await.unwrap();
    let response = responses.next().await.unwrap().unwrap();

    let content = response.content.unwrap();
    assert!(matches!(content.parts.as_slice(), [Part::Text { text }] if text == "ok"));
    let arrivals = arrivals.lock().unwrap();
    let waited = arrivals[1].duration_since(arrivals[0]);
    assert!(waited >= Duration::from_millis(400), "retried after {waited:?}");
}

#[tokio::test]
async fn carries_the_retry_delay_on_the_error() {
    for stream in [false, true] {
        for (delay, expected) in [("12s", 12_000), ("1.5s", 1_500), ("0.25s", 250)] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(quota_exhausted(delay))
                .expect(1)
                .mount(&server)
                .await;
            let model = model(&server, RetryConfig::disabled());
            let error = match model.generate_content(request(), stream).await {
                Err(error) => error,
                Ok(mut responses) => responses.next().await.unwrap().unwrap_err(),
            };
            assert_eq!(error.category, ErrorCategory::RateLimited, "{delay}");
            assert_eq!(error.details.upstream_status_code, Some(429));
            assert_eq!(
                error.retry.retry_after(),
                Some(Duration::from_millis(expected)),
                "stream {stream}, {delay}"
            );
        }
    }
}

#[tokio::test]
async fn a_delay_beyond_the_policy_maximum_is_not_waited() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(quota_exhausted("45s")).expect(1).mount(&server).await;
    let retry = RetryConfig::default().with_max_server_delay(Duration::from_secs(1));

    let started = Instant::now();
    let error = match model(&server, retry).generate_content(request(), false).await {
        Err(error) => error,
        Ok(mut responses) => responses.next().await.unwrap().unwrap_err(),
    };

    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(45)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
