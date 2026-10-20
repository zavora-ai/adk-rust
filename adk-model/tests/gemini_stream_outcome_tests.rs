//! Gemini responses that carry an error, end early, or are blocked do not read as an
//! empty success.

#![cfg(feature = "gemini")]

use adk_core::{AdkError, Content, ErrorCategory, FinishReason, Llm, LlmRequest, LlmResponse};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "gemini-3.7-flash";

fn sse(events: &[Value]) -> ResponseTemplate {
    let body: String = events.iter().map(|event| format!("data: {event}\r\n\r\n")).collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

fn chunk(text: Option<&str>, finish: Option<&str>) -> Value {
    let mut candidate = json!({"index": 0});
    if let Some(text) = text {
        candidate["content"] = json!({"role": "model", "parts": [{"text": text}]});
    }
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    json!({"candidates": [candidate], "modelVersion": MODEL})
}

fn overloaded() -> Value {
    json!({"error": {"code": 503, "message": "The model is overloaded.", "status": "UNAVAILABLE"}})
}

fn blocked() -> Value {
    json!({
        "promptFeedback": {"blockReason": "PROHIBITED_CONTENT"},
        "usageMetadata": {"promptTokenCount": 8, "totalTokenCount": 8}
    })
}

/// Every item a call yields, stopping at the first error.
async fn generate(reply: ResponseTemplate, stream: bool) -> (Vec<LlmResponse>, Option<AdkError>) {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(reply).mount(&server).await;
    let model =
        GeminiModel::new_with_base_url("test-key", MODEL, format!("{}/v1beta/", server.uri()))
            .unwrap()
            .with_retry_config(adk_model::retry::RetryConfig::disabled());
    let request = LlmRequest::new(MODEL, vec![Content::new("user").with_text("Hello")]);
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

fn provider_code(error: &AdkError) -> Option<&str> {
    error.details.metadata.get("provider_error_code").and_then(Value::as_str)
}

#[tokio::test]
async fn error_object_mid_stream_fails_the_stream() {
    let (responses, error) =
        generate(sse(&[chunk(Some("The capital "), None), overloaded()]), true).await;
    let error = error.expect("an error object must fail the stream");
    assert_eq!(
        (error.category, error.code, provider_code(&error), error.details.upstream_status_code),
        (ErrorCategory::Unavailable, "model.gemini.unavailable", Some("UNAVAILABLE"), None)
    );
    assert!(error.is_retryable());
    assert!(responses.iter().all(|response| response.partial), "{responses:?}");
}

#[tokio::test]
async fn error_object_in_a_successful_body_is_an_error() {
    let (responses, error) =
        generate(ResponseTemplate::new(200).set_body_json(overloaded()), false).await;
    let error = error.expect("an error object must not decode as an empty response");
    assert_eq!((error.category, responses.len()), (ErrorCategory::Unavailable, 0));

    let rate_limited = json!({"error": {"code": 429, "message": "Quota exceeded.",
        "status": "RESOURCE_EXHAUSTED"}});
    let (_, error) = generate(sse(&[rate_limited]), true).await;
    assert_eq!(error.map(|error| error.category), Some(ErrorCategory::RateLimited));
}

#[tokio::test]
async fn stream_without_a_finish_reason_fails() {
    let (responses, error) =
        generate(sse(&[chunk(Some("The capital "), None), chunk(Some("is Par"), None)]), true)
            .await;
    let error = error.expect("a stream cut before finishReason must fail");
    assert_eq!(
        (error.category, error.code),
        (ErrorCategory::Unavailable, "model.gemini.unfinished_stream")
    );
    assert_eq!(responses.len(), 2);

    let (responses, error) = generate(
        sse(&[chunk(Some("The capital "), None), chunk(Some("is Paris."), Some("STOP"))]),
        true,
    )
    .await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        responses.last().and_then(|response| response.finish_reason),
        Some(FinishReason::Stop)
    );
}

#[tokio::test]
async fn blocked_prompt_is_a_safety_turn_with_an_error_code() {
    for (reply, stream) in
        [(sse(&[blocked()]), true), (ResponseTemplate::new(200).set_body_json(blocked()), false)]
    {
        let (responses, error) = generate(reply, stream).await;
        assert!(error.is_none(), "{error:?}");
        let last = responses.last().expect("a blocked prompt yields its terminal response");
        assert_eq!(
            (
                last.finish_reason,
                last.error_code.as_deref(),
                last.error_message.as_deref(),
                last.partial,
                last.turn_complete,
                last.content.is_none()
            ),
            (
                Some(FinishReason::Safety),
                Some("PROHIBITED_CONTENT"),
                Some("Gemini blocked the prompt (blockReason PROHIBITED_CONTENT)"),
                false,
                true,
                true
            ),
            "stream={stream}"
        );
    }
}

#[tokio::test]
async fn block_type_finish_reasons_are_safety() {
    for (reason, expected) in [
        ("SAFETY", FinishReason::Safety),
        ("BLOCKLIST", FinishReason::Safety),
        ("PROHIBITED_CONTENT", FinishReason::Safety),
        ("SPII", FinishReason::Safety),
        ("IMAGE_SAFETY", FinishReason::Safety),
        ("MODEL_ARMOR", FinishReason::Safety),
        ("RECITATION", FinishReason::Recitation),
        ("LANGUAGE", FinishReason::Other),
        ("MALFORMED_FUNCTION_CALL", FinishReason::Other),
    ] {
        let reply = ResponseTemplate::new(200).set_body_json(chunk(None, Some(reason)));
        let (responses, error) = generate(reply, false).await;
        assert!(error.is_none(), "{reason}: {error:?}");
        assert_eq!(
            (
                responses.last().and_then(|response| response.finish_reason),
                responses.last().and_then(|r| r.error_code.clone())
            ),
            (Some(expected), None),
            "{reason}"
        );
    }
}

#[cfg(feature = "gemini-interactions")]
mod interactions {
    use super::*;

    async fn stream(events: &[Value]) -> (Vec<LlmResponse>, Option<AdkError>) {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(sse(events)).mount(&server).await;
        let model =
            GeminiModel::new_with_base_url("test-key", MODEL, format!("{}/v1beta/", server.uri()))
                .unwrap()
                .with_retry_config(adk_model::retry::RetryConfig::disabled())
                .use_interactions_api(true)
                .unwrap();
        let request = LlmRequest::new(MODEL, vec![Content::new("user").with_text("Hello")]);
        let mut responses = Vec::new();
        let mut stream = model.generate_content(request, true).await.unwrap();
        while let Some(item) = stream.next().await {
            match item {
                Ok(response) => responses.push(response),
                Err(error) => return (responses, Some(error)),
            }
        }
        (responses, None)
    }

    fn created() -> Value {
        json!({"event_type": "interaction.created",
            "interaction": {"id": "int_1", "status": "in_progress"}})
    }

    fn delta(text: &str) -> Value {
        json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": text}})
    }

    fn completed(status: &str) -> Value {
        json!({"event_type": "interaction.completed",
            "interaction": {"id": "int_1", "status": status}})
    }

    #[tokio::test]
    async fn stream_without_interaction_completed_fails() {
        let (responses, error) = stream(&[created(), delta("The capital")]).await;
        let error = error.expect("a stream without interaction.completed must fail");
        assert_eq!(
            (error.category, error.code, responses.len()),
            (ErrorCategory::Unavailable, "model.gemini.interactions.unfinished_stream", 1)
        );
    }

    #[tokio::test]
    async fn failed_status_fails_the_stream() {
        let status_update = json!({"event_type": "interaction.status_update",
            "interaction_id": "int_1", "status": "failed"});
        for terminal in [completed("failed"), completed("budget_exceeded"), status_update] {
            let (_, error) = stream(&[created(), delta("The capital"), terminal.clone()]).await;
            assert!(
                error
                    .as_ref()
                    .is_some_and(|error| error.code.starts_with("model.gemini.interactions.")),
                "{terminal}: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn completed_interaction_ends_the_stream() {
        let (responses, error) =
            stream(&[created(), delta("The capital is Paris."), completed("completed")]).await;
        assert!(error.is_none(), "{error:?}");
        let last = responses.last().unwrap();
        assert_eq!((last.partial, last.finish_reason), (false, Some(FinishReason::Stop)));
    }
}

#[tokio::test]
async fn preserves_http_retry_hints() {
    use std::time::{Duration, SystemTime};

    let date = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(90));
    for stream in [false, true] {
        for status in [400, 429, 503] {
            for header in [Some("7"), Some("61"), Some(date.as_str()), Some("invalid"), None] {
                let server = MockServer::start().await;
                let mut reply = ResponseTemplate::new(status).set_body_json(json!({
                    "error": {"code": status, "message": "Fixture error"}
                }));
                if let Some(value) = header {
                    reply = reply.insert_header("retry-after", value);
                }
                Mock::given(method("POST")).respond_with(reply).expect(1).mount(&server).await;
                let model = GeminiModel::new_with_base_url(
                    "fixture-key",
                    MODEL,
                    format!("{}/v1beta/", server.uri()),
                )
                .unwrap()
                .with_retry_config(adk_model::retry::RetryConfig::disabled());
                let request = LlmRequest::new(MODEL, vec![Content::new("user").with_text("Hello")]);
                let error = match model.generate_content(request, stream).await {
                    Err(error) => error,
                    Ok(mut responses) => responses.next().await.unwrap().unwrap_err(),
                };
                assert_eq!(error.details.upstream_status_code, Some(status));
                if status == 400 || header.is_none() || header == Some("invalid") {
                    assert_eq!(error.retry.retry_after(), None);
                } else if header == Some(date.as_str()) {
                    let delay = error.retry.retry_after().unwrap();
                    assert!(delay > Duration::from_secs(60) && delay <= Duration::from_secs(90));
                } else {
                    let seconds = if header == Some("7") { 7 } else { 61 };
                    assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(seconds)));
                }
                assert_eq!(server.received_requests().await.unwrap().len(), 1);
            }
        }
    }
}
