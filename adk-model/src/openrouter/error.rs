//! Typed OpenRouter error payloads.

use crate::openrouter::stream::OpenRouterStreamError;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// Top-level OpenRouter error envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenRouterErrorEnvelope {
    pub error: OpenRouterErrorBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// OpenRouter error body preserved from upstream responses and stream events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenRouterErrorBody {
    /// Human-readable error message.
    pub message: String,
    /// Optional provider or platform-specific error type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Optional provider error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    /// Optional request parameter name associated with the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// Optional upstream provider identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// OpenRouter sometimes includes additional structured metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Forward-compatible carrier for newly added error fields.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Normalize an HTTP OpenRouter error response into `AdkError`.
pub fn api_error_to_adk_error(status_code: u16, headers: &HeaderMap, body: &str) -> AdkError {
    let parsed = serde_json::from_str::<OpenRouterErrorEnvelope>(body).ok();
    let (category, code) = status_category_and_code(status_code);
    let message = parsed
        .as_ref()
        .map(|payload| payload.error.message.clone())
        .unwrap_or_else(|| format!("OpenRouter request failed with status {status_code}"));

    let mut error = AdkError::new(ErrorComponent::Model, category, code, message)
        .with_provider("openrouter")
        .with_upstream_status(status_code);

    if let Some(request_id) = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .or_else(|| headers.get("request-id").and_then(|value| value.to_str().ok()))
    {
        error = error.with_request_id(request_id.to_string());
    }

    if let Some(payload) = parsed.as_ref() {
        error = attach_error_payload_details(error, payload);
    }

    if let Some(retry_after) = retry_after_from_headers(headers)
        .or_else(|| parsed.as_ref().and_then(retry_after_from_error_payload))
    {
        let retry_hint = error.retry.clone().with_retry_after(retry_after);
        error = error.with_retry(retry_hint);
    }

    error
}

/// Normalize an OpenRouter stream error event into `AdkError`.
pub fn stream_error_to_adk_error(
    stream_error: &OpenRouterStreamError,
    sse_retry_ms: Option<u64>,
) -> AdkError {
    let status_code = stream_error.code.as_ref().and_then(json_status_code);
    let (category, code) = status_code
        .map(status_category_and_code)
        .unwrap_or((ErrorCategory::Internal, "model.openrouter.stream_error"));

    let mut error =
        AdkError::new(ErrorComponent::Model, category, code, stream_error.message.clone())
            .with_provider(
                stream_error.provider_name.clone().unwrap_or_else(|| "openrouter".to_string()),
            );

    if let Some(status_code) = status_code {
        error = error.with_upstream_status(status_code);
    }

    let mut metadata = Map::new();
    if let Some(code_value) = stream_error.code.clone() {
        metadata.insert("code".to_string(), code_value);
    }
    if let Some(error_type) = stream_error.error_type.as_ref() {
        metadata.insert("type".to_string(), Value::String(error_type.clone()));
    }
    if let Some(param) = stream_error.param.as_ref() {
        metadata.insert("param".to_string(), Value::String(param.clone()));
    }
    if let Some(provider_name) = stream_error.provider_name.as_ref() {
        metadata.insert("provider_name".to_string(), Value::String(provider_name.clone()));
    }
    if let Some(sequence_number) = stream_error.sequence_number {
        metadata.insert("sequence_number".to_string(), json!(sequence_number));
    }
    if let Some(provider_metadata) = stream_error.metadata.clone() {
        metadata.insert("metadata".to_string(), provider_metadata);
    }
    if !stream_error.extra.is_empty() {
        metadata.insert("extra".to_string(), json!(stream_error.extra));
    }
    if !metadata.is_empty() {
        error.details.metadata.extend(metadata);
    }

    if let Some(retry_after_ms) = sse_retry_ms {
        let retry_hint =
            error.retry.clone().with_retry_after(Duration::from_millis(retry_after_ms));
        error = error.with_retry(retry_hint);
    } else if let Some(retry_after) =
        stream_error.metadata.as_ref().and_then(retry_after_from_metadata_value)
    {
        let retry_hint = error.retry.clone().with_retry_after(retry_after);
        error = error.with_retry(retry_hint);
    }

    error
}

fn status_category_and_code(status_code: u16) -> (ErrorCategory, &'static str) {
    match status_code {
        400 => (ErrorCategory::InvalidInput, "model.openrouter.bad_request"),
        401 => (ErrorCategory::Unauthorized, "model.openrouter.unauthorized"),
        402 => (ErrorCategory::Forbidden, "model.openrouter.insufficient_credits"),
        403 => (ErrorCategory::Forbidden, "model.openrouter.forbidden"),
        404 => (ErrorCategory::NotFound, "model.openrouter.not_found"),
        408 => (ErrorCategory::Timeout, "model.openrouter.timeout"),
        413 => (ErrorCategory::InvalidInput, "model.openrouter.payload_too_large"),
        422 => (ErrorCategory::InvalidInput, "model.openrouter.unprocessable_entity"),
        429 => (ErrorCategory::RateLimited, "model.openrouter.rate_limited"),
        500 => (ErrorCategory::Internal, "model.openrouter.internal"),
        502 => (ErrorCategory::Unavailable, "model.openrouter.bad_gateway"),
        503 => (ErrorCategory::Unavailable, "model.openrouter.unavailable"),
        524 => (ErrorCategory::Timeout, "model.openrouter.edge_timeout"),
        529 => (ErrorCategory::Unavailable, "model.openrouter.provider_overloaded"),
        _ => (ErrorCategory::Internal, "model.openrouter.api_error"),
    }
}

fn attach_error_payload_details(
    mut error: AdkError,
    payload: &OpenRouterErrorEnvelope,
) -> AdkError {
    if let Some(provider_name) = payload.error.provider_name.as_ref() {
        error = error.with_provider(provider_name.clone());
    }

    let mut metadata = Map::new();
    if let Some(error_type) = payload.error.r#type.as_ref() {
        metadata.insert("type".to_string(), Value::String(error_type.clone()));
    }
    if let Some(code) = payload.error.code.clone() {
        metadata.insert("code".to_string(), code);
    }
    if let Some(param) = payload.error.param.as_ref() {
        metadata.insert("param".to_string(), Value::String(param.clone()));
    }
    if let Some(provider_name) = payload.error.provider_name.as_ref() {
        metadata.insert("provider_name".to_string(), Value::String(provider_name.clone()));
    }
    if let Some(extra_metadata) = payload.error.metadata.clone() {
        metadata.insert("metadata".to_string(), extra_metadata);
    }
    if let Some(user_id) = payload.user_id.as_ref() {
        metadata.insert("user_id".to_string(), Value::String(user_id.clone()));
    }
    if !payload.error.extra.is_empty() {
        metadata.insert("extra".to_string(), json!(payload.error.extra));
    }

    if !metadata.is_empty() {
        error.details.metadata.extend(metadata);
    }

    error
}

fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_value)
}

fn retry_after_from_error_payload(payload: &OpenRouterErrorEnvelope) -> Option<Duration> {
    payload.error.metadata.as_ref().and_then(retry_after_from_metadata_value)
}

fn parse_retry_after_value(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = DateTime::parse_from_rfc2822(value).ok()?.with_timezone(&Utc);
    let delay = retry_at.signed_duration_since(Utc::now()).to_std().ok()?;
    (!delay.is_zero()).then_some(delay)
}

fn json_status_code(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|code| u16::try_from(code).ok())
        .or_else(|| value.as_i64().and_then(|code| u16::try_from(code).ok()))
}

fn retry_after_from_metadata_value(metadata: &Value) -> Option<Duration> {
    if let Some(retry_after_ms) = metadata
        .get("retry_after_ms")
        .or_else(|| metadata.get("retryAfterMs"))
        .and_then(Value::as_u64)
    {
        return Some(Duration::from_millis(retry_after_ms));
    }

    metadata.get("retry_after").or_else(|| metadata.get("retryAfter")).and_then(|value| match value
    {
        Value::Number(number) => number.as_u64().map(Duration::from_secs),
        Value::String(text) => parse_retry_after_value(text),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::{api_error_to_adk_error, parse_retry_after_value, stream_error_to_adk_error};
    use crate::openrouter::stream::OpenRouterStreamError;
    use adk_core::ErrorCategory;
    use chrono::Utc;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use serde_json::json;

    #[test]
    fn http_statuses_normalize_to_expected_error_categories_and_codes() {
        let cases = [
            (402, ErrorCategory::Forbidden, "model.openrouter.insufficient_credits"),
            (429, ErrorCategory::RateLimited, "model.openrouter.rate_limited"),
            (503, ErrorCategory::Unavailable, "model.openrouter.unavailable"),
            (524, ErrorCategory::Timeout, "model.openrouter.edge_timeout"),
            (529, ErrorCategory::Unavailable, "model.openrouter.provider_overloaded"),
        ];

        for (status, category, code) in cases {
            let error = api_error_to_adk_error(
                status,
                &HeaderMap::new(),
                &json!({
                    "error": {
                        "message": format!("status {status}"),
                        "code": status,
                        "provider_name": "openrouter"
                    },
                    "user_id": "user_123"
                })
                .to_string(),
            );

            assert_eq!(error.category, category);
            assert_eq!(error.code, code);
            assert_eq!(error.details.upstream_status_code, Some(status));
            assert_eq!(error.details.provider.as_deref(), Some("openrouter"));
            assert_eq!(error.details.metadata.get("user_id"), Some(&json!("user_123")));
        }
    }

    #[test]
    fn retry_after_header_is_honored_for_seconds_and_http_dates() {
        let mut seconds_headers = HeaderMap::new();
        seconds_headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));

        let seconds_error = api_error_to_adk_error(
            429,
            &seconds_headers,
            &json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "code": 429
                }
            })
            .to_string(),
        );
        assert_eq!(seconds_error.retry.retry_after_ms, Some(12_000));

        let future_value =
            (Utc::now() + chrono::TimeDelta::seconds(30)).to_rfc2822().replace("+0000", "GMT");
        let mut date_headers = HeaderMap::new();
        date_headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&future_value).expect("date header should be valid"),
        );

        let date_error = api_error_to_adk_error(
            429,
            &date_headers,
            &json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "code": 429
                }
            })
            .to_string(),
        );
        assert!(date_error.retry.retry_after_ms.is_some_and(|delay| delay > 0));
        assert!(parse_retry_after_value(&future_value).is_some());
    }

    #[test]
    fn stream_errors_normalize_to_adk_errors() {
        let error = stream_error_to_adk_error(
            &OpenRouterStreamError {
                message: "Provider overloaded".to_string(),
                code: Some(json!(529)),
                param: Some("model".to_string()),
                error_type: Some("provider_overloaded".to_string()),
                provider_name: Some("openrouter".to_string()),
                metadata: Some(json!({ "retry_after_ms": 1500 })),
                sequence_number: Some(7),
                ..Default::default()
            },
            None,
        );

        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.code, "model.openrouter.provider_overloaded");
        assert_eq!(error.details.upstream_status_code, Some(529));
        assert_eq!(error.details.metadata.get("sequence_number"), Some(&json!(7)));
        assert_eq!(error.retry.retry_after_ms, Some(1500));
    }
}
