//! Provider errors keep request content out of logs, and error messages quote a bounded
//! excerpt of the response body.
//!
//! A provider's 400 response can echo the prompt, a tool argument, or an attachment's
//! metadata. The clients logged that body or message at ERROR or WARN, and the Gemini client
//! logged every non-streaming response at TRACE, whatever the run's
//! `RunConfig::record_payloads`; error messages carried the whole body.
#![cfg(any(
    feature = "openai",
    feature = "gemini",
    feature = "anthropic",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "ollama",
    feature = "openrouter"
))]

use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, UPSTREAM_EXCERPT_BYTES};
use futures::StreamExt;
use serde_json::json;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "acct-7731-secret";
const TAIL: &str = "TAIL-OF-A-LONG-BODY";

/// Every span field and event field, at every level.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<String>>>);

struct Values<'a>(&'a mut Vec<String>);

impl Visit for Values<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push(format!("{}={value}", field.name()));
    }
}

impl<S> Layer<S> for Recorded
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        attrs.record(&mut Values(&mut self.0.lock().unwrap()));
    }

    fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        values.record(&mut Values(&mut self.0.lock().unwrap()));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        event.record(&mut Values(&mut self.0.lock().unwrap()));
    }
}

impl Recorded {
    fn leaks(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().filter(|value| value.contains(SECRET)).cloned().collect()
    }
}

/// An error message that echoes the secret, then runs far past the excerpt bound.
fn echoed() -> String {
    format!("Invalid value: '{SECRET}'. {}{TAIL}", "x".repeat(4096))
}

/// The error object every OpenAI-, Google-, and Anthropic-shaped API sends.
fn echoing_body() -> serde_json::Value {
    json!({"type": "error", "error": {
        "message": echoed(),
        "type": "invalid_request_error",
        "code": 400,
        "status": "INVALID_ARGUMENT",
    }})
}

async fn upstream(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// What one call left: the first error, or an error reported inside a response.
struct Failure {
    message: String,
    logs: Recorded,
}

async fn fail(model: &dyn Llm, stream: bool) -> Failure {
    let logs = Recorded::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let request =
        LlmRequest::new("model", vec![Content::new("user").with_text("Check my account.")]);
    let message = match model.generate_content(request, stream).await {
        Err(error) => error.message,
        Ok(mut responses) => loop {
            match responses.next().await {
                Some(Err(error)) => break error.message,
                Some(Ok(response)) if response.error_message.is_some() => {
                    break response.error_message.unwrap_or_default();
                }
                Some(Ok(_)) => continue,
                None => panic!("the call did not fail"),
            }
        },
    };
    Failure { message, logs }
}

fn assert_private_and_bounded(provider: &str, failure: &Failure) {
    let leaks = failure.logs.leaks();
    assert!(leaks.is_empty(), "{provider}: the echoed body reached tracing: {leaks:#?}");
    assert!(
        !failure.message.contains(TAIL) && failure.message.len() <= UPSTREAM_EXCERPT_BYTES + 128,
        "{provider}: the error message is not bounded ({} bytes)",
        failure.message.len()
    );
    assert!(
        failure.message.contains(SECRET),
        "{provider}: the excerpt keeps the start of the provider's message: {}",
        failure.message
    );
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_compatible_http_errors_are_bounded_and_not_logged() {
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model = OpenAICompatible::new(
        OpenAICompatibleConfig::new("key", "model").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("openai-compatible", &fail(&model, true).await);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_responses_http_errors_are_bounded_and_not_logged() {
    use adk_model::RetryConfig;
    use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};

    // A string `code`, so the body parses as OpenAI's error object and its message is quoted.
    let body = json!({"error": {"message": echoed(), "type": "invalid_request_error", "code": "invalid_value"}});
    let server = upstream(400, body).await;
    let model = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("key", "gpt-5.6-terra").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("openai-responses", &fail(&model, true).await);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn retried_errors_are_logged_without_their_message() {
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig, RetryConfig};
    use std::time::Duration;

    let server = upstream(503, echoing_body()).await;
    let model = OpenAICompatible::new(
        OpenAICompatibleConfig::new("key", "model").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(
        RetryConfig::default()
            .with_max_retries(1)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO),
    );
    let failure = fail(&model, true).await;
    assert_private_and_bounded("openai-compatible retry", &failure);
    let logged = failure.logs.0.lock().unwrap().clone();
    assert!(
        logged
            .iter()
            .any(|value| value.starts_with("error=model.unavailable")
                && value.contains("status 503")),
        "the retry is logged with its diagnostics: {logged:#?}"
    );
}

#[cfg(feature = "groq")]
#[tokio::test]
async fn groq_http_errors_are_bounded_and_not_logged() {
    use adk_model::{GroqClient, GroqConfig, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model = GroqClient::new(GroqConfig::new("key", "model").with_base_url(server.uri()))
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("groq", &fail(&model, true).await);
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn deepseek_http_errors_are_bounded_and_not_logged() {
    use adk_model::{DeepSeekClient, DeepSeekConfig, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model =
        DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(server.uri()))
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("deepseek", &fail(&model, true).await);
}

#[cfg(feature = "azure-ai")]
#[tokio::test]
async fn azure_ai_http_errors_are_bounded_and_not_logged() {
    use adk_model::{AzureAIClient, AzureAIConfig, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model = AzureAIClient::new(AzureAIConfig::new(server.uri(), "key", "model"))
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("azure-ai", &fail(&model, true).await);
}

#[cfg(feature = "ollama")]
#[tokio::test]
async fn ollama_http_errors_are_bounded_and_not_logged() {
    use adk_model::{OllamaConfig, OllamaModel};

    let server = upstream(400, json!({"error": echoed()})).await;
    let model = OllamaModel::new(OllamaConfig::with_host(server.uri(), "qwen3.5")).unwrap();
    assert_private_and_bounded("ollama stream", &fail(&model, true).await);
    assert_private_and_bounded("ollama", &fail(&model, false).await);
}

#[cfg(feature = "openrouter")]
#[tokio::test]
async fn openrouter_http_errors_are_bounded_and_not_logged() {
    use adk_model::{OpenRouterClient, OpenRouterConfig, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model = OpenRouterClient::new(
        OpenRouterConfig::new("key", "openai/gpt-4.1-mini").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("openrouter", &fail(&model, true).await);
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_http_errors_are_bounded_and_not_logged() {
    use adk_model::{GeminiModel, RetryConfig};

    let server = upstream(400, echoing_body()).await;
    let model = GeminiModel::new_with_base_url(
        "key",
        "gemini-3.5-flash-lite",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("gemini stream", &fail(&model, true).await);
    assert_private_and_bounded("gemini", &fail(&model, false).await);
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_responses_are_not_logged() {
    use adk_model::GeminiModel;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": format!("Account {SECRET} is active.")}]},
                "finishReason": "STOP",
                "index": 0,
            }],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 5, "totalTokenCount": 10},
        })))
        .mount(&server)
        .await;
    let model = GeminiModel::new_with_base_url(
        "key",
        "gemini-3.5-flash-lite",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();

    let logs = Recorded::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(logs.clone()));
    let request = LlmRequest::new("model", vec![Content::new("user").with_text("Hello")]);
    let mut responses = model.generate_content(request, false).await.unwrap();
    while let Some(response) = responses.next().await {
        response.unwrap();
    }
    let leaks = logs.leaks();
    assert!(leaks.is_empty(), "the response reached tracing: {leaks:#?}");
}

#[cfg(feature = "anthropic")]
#[tokio::test]
async fn anthropic_http_errors_are_bounded_and_not_logged() {
    use adk_model::RetryConfig;
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

    let server = upstream(400, echoing_body()).await;
    let model = AnthropicClient::new(
        AnthropicConfig::new("key", "claude-sonnet-4-6").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    assert_private_and_bounded("anthropic stream", &fail(&model, true).await);
    assert_private_and_bounded("anthropic", &fail(&model, false).await);
}

#[cfg(feature = "deepseek")]
#[tokio::test]
async fn undecodable_responses_are_described_without_their_content() {
    use adk_model::{DeepSeekClient, DeepSeekConfig, RetryConfig};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "c", "object": "chat.completion", "created": SECRET})),
        )
        .mount(&server)
        .await;
    let model =
        DeepSeekClient::new(DeepSeekConfig::new("key", "model").with_base_url(server.uri()))
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
    let failure = fail(&model, false).await;
    assert!(!failure.message.contains(SECRET), "{}", failure.message);
    assert!(failure.message.contains("schema error"), "{}", failure.message);
}

/// The message on the `AdkError` itself is what callers log; it is checked directly too.
#[cfg(feature = "anthropic")]
#[test]
fn anthropic_api_errors_quote_a_bounded_message() {
    use adk_core::AdkError;
    use adk_model::anthropic::AnthropicApiError;

    let error: AdkError = AnthropicApiError {
        error_type: "invalid_request_error".into(),
        message: format!("'{SECRET}' {}{TAIL}", "x".repeat(4096)),
        status_code: 400,
        request_id: Some("req_1".into()),
    }
    .into();
    assert!(!error.message.contains(TAIL));
    assert!(error.message.contains("[request_id=req_1]"), "{}", error.message);
}
