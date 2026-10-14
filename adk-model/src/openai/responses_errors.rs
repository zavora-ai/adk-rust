//! Responses HTTP error normalization and retry metadata.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_openai::error::{ApiError, ApiErrorResponse, OpenAIError, WrappedError};
use std::{fmt, time::Duration};

use crate::retry::{parse_retry_after, with_retry_after};

/// Longest raw error body quoted in an error message.
const BODY_EXCERPT_BYTES: usize = 512;

struct HttpError {
    error: ApiErrorResponse,
    retry_after: Option<Duration>,
    request_id: Option<String>,
}

impl fmt::Debug for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpError")
            .field("status", &self.error.status_code.as_u16())
            .field("retry_after", &self.retry_after)
            .field("request_id", &self.request_id)
            .finish()
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "OpenAI Responses HTTP {}", self.error.status_code.as_u16())
    }
}

impl std::error::Error for HttpError {}

/// Decode a failed HTTP response before the SDK can replace its status with a
/// body-deserialization error. `Retry-After` and `x-request-id` are captured
/// before consuming the body.
pub(super) async fn normalize_response(
    response: reqwest_openai::Response,
    open_responses_mode: bool,
) -> Result<reqwest_openai::Response, OpenAIError> {
    let status = response.status();
    let header = |name| response.headers().get(name).and_then(|value| value.to_str().ok());
    let retry_after = header(http::header::RETRY_AFTER).and_then(parse_retry_after);
    let request_id = header(http::HeaderName::from_static("x-request-id"))
        .and_then(identifier)
        .map(str::to_owned);
    let api_error = match response.bytes().await {
        Ok(bytes) => api_error(status, &bytes, open_responses_mode),
        Err(error) => {
            tracing::debug!(error = %error, http.status = status.as_u16(), "failed to read responses error body");
            ApiError {
                message: format!("HTTP {status} (error body could not be read)"),
                r#type: None,
                param: None,
                code: None,
            }
        }
    };
    Err(OpenAIError::Boxed(Box::new(HttpError {
        error: ApiErrorResponse { status_code: status, api_error },
        retry_after,
        request_id,
    })))
}

fn api_error(
    status: reqwest_openai::StatusCode,
    bytes: &[u8],
    open_responses_mode: bool,
) -> ApiError {
    if let Ok(wrapped) = serde_json::from_slice::<WrappedError>(bytes) {
        return wrapped.error;
    }
    if open_responses_mode
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
        && value.get("error").is_none()
        && let Some(detail) = value.get("detail").and_then(serde_json::Value::as_str)
    {
        return ApiError {
            message: detail.to_owned(),
            r#type: None,
            param: None,
            code: value.get("code").and_then(serde_json::Value::as_str).map(str::to_owned),
        };
    }
    let excerpt = String::from_utf8_lossy(&bytes[..bytes.len().min(BODY_EXCERPT_BYTES)]);
    let excerpt = excerpt.trim();
    let message = if excerpt.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {excerpt}")
    };
    ApiError { message, r#type: None, param: None, code: None }
}

/// Returns `value` when it is short and limited to ASCII alphanumerics, `_`, `.`, and `-`.
fn identifier(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)))
    .then_some(value)
}

pub(super) fn mapped_http_error(error: &OpenAIError) -> Option<AdkError> {
    let OpenAIError::Boxed(error) = error else {
        return None;
    };
    let error = error.downcast_ref::<HttpError>()?;
    // A delay above `RetryConfig::max_delay` is capped by the retry loop.
    let mapped = with_retry_after(map_api_error(&error.error), error.retry_after);
    Some(match &error.request_id {
        Some(request_id) => mapped.with_request_id(request_id.as_str()),
        None => mapped,
    })
}

pub(super) fn map_api_error(api_err: &ApiErrorResponse) -> AdkError {
    let status_code = api_err.status_code.as_u16();
    let (category, code) = match status_code {
        401 => (ErrorCategory::Unauthorized, "model.openai_responses.unauthorized"),
        403 => (ErrorCategory::Forbidden, "model.openai_responses.forbidden"),
        404 => (ErrorCategory::NotFound, "model.openai_responses.not_found"),
        408 => (ErrorCategory::Timeout, "model.openai_responses.timeout"),
        429 => (ErrorCategory::RateLimited, "model.openai_responses.rate_limited"),
        500 | 502 | 503 | 504 | 529 => {
            (ErrorCategory::Unavailable, "model.openai_responses.unavailable")
        }
        400..=499 => (ErrorCategory::InvalidInput, "model.openai_responses.invalid_request"),
        _ => (ErrorCategory::Internal, "model.openai_responses.api_error"),
    };
    let mut mapped = AdkError::new(
        ErrorComponent::Model,
        category,
        code,
        format!("OpenAI Responses API error: {api_err}"),
    )
    .with_provider("openai-responses")
    .with_upstream_status(status_code);
    if let Some(provider_code) = api_err.api_error.code.as_deref().and_then(identifier) {
        mapped.details.metadata.insert(
            "provider_error_code".to_string(),
            serde_json::Value::String(provider_code.to_string()),
        );
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_a_bounded_excerpt_of_unrecognized_bodies() {
        let status = reqwest_openai::StatusCode::BAD_GATEWAY;
        assert_eq!(
            api_error(status, b"  upstream timed out \n", false).message,
            "HTTP 502 Bad Gateway: upstream timed out"
        );
        assert_eq!(api_error(status, b"", false).message, "HTTP 502 Bad Gateway");
        let long = "x".repeat(BODY_EXCERPT_BYTES * 2);
        let message = api_error(status, long.as_bytes(), false).message;
        assert_eq!(message.len(), "HTTP 502 Bad Gateway: ".len() + BODY_EXCERPT_BYTES);
    }

    #[test]
    fn accepts_only_identifier_like_values() {
        assert_eq!(identifier("req_123-abc.def"), Some("req_123-abc.def"));
        for value in ["", "secret token", "line\nbreak", &"a".repeat(129)] {
            assert_eq!(identifier(value), None, "{value:?}");
        }
    }

    #[test]
    fn preserves_safe_provider_codes() {
        let error = ApiErrorResponse {
            status_code: reqwest_openai::StatusCode::BAD_REQUEST,
            api_error: ApiError {
                message: "invalid".into(),
                r#type: None,
                param: None,
                code: Some("invalid_schema".into()),
            },
        };
        let mapped = map_api_error(&error);
        assert_eq!(mapped.details.upstream_status_code, Some(400));
        assert_eq!(mapped.details.metadata["provider_error_code"], "invalid_schema");
    }

    #[test]
    fn drops_unsafe_provider_codes() {
        let error = ApiErrorResponse {
            status_code: reqwest_openai::StatusCode::BAD_REQUEST,
            api_error: ApiError {
                message: "invalid".into(),
                r#type: None,
                param: None,
                code: Some("secret token\nvalue".into()),
            },
        };
        let mapped = map_api_error(&error);
        assert!(!mapped.details.metadata.contains_key("provider_error_code"));
    }
}
