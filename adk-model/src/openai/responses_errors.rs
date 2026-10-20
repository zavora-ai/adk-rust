//! Responses HTTP error normalization and retry metadata.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_openai::error::{ApiError, ApiErrorResponse, OpenAIError, WrappedError};
use std::{fmt, time::Duration};

use crate::retry::{parse_retry_after, with_retry_after};

struct HttpError {
    error: ApiErrorResponse,
    retry_after: Option<Duration>,
    request_id: Option<String>,
    /// The provider's code, read from the raw body by the rule every client shares.
    provider_code: Option<String>,
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
    let (api_error, provider_code) = match response.bytes().await {
        Ok(bytes) => (
            api_error(status, &bytes, open_responses_mode),
            crate::error_metadata::code_from_body(&String::from_utf8_lossy(&bytes)),
        ),
        Err(error) => {
            tracing::debug!(error = %error, http.status = status.as_u16(), "failed to read responses error body");
            let api_error = ApiError {
                message: format!("HTTP {status} (error body could not be read)"),
                r#type: None,
                param: None,
                code: None,
            };
            (api_error, None)
        }
    };
    Err(OpenAIError::Boxed(Box::new(HttpError {
        error: ApiErrorResponse { status_code: status, api_error },
        retry_after,
        request_id,
        provider_code,
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
    let body = String::from_utf8_lossy(bytes);
    let excerpt = adk_core::upstream_excerpt(&body);
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
    // The retry owner bounds the wait while retaining the original server hint.
    let mut mapped = with_retry_after(map_api_error(&error.error), error.retry_after);
    // The body's `status`, and a `code` that is not a string, do not survive `ApiError`.
    if error.provider_code.is_some() {
        mapped = crate::error_metadata::with_code(mapped, error.provider_code.as_deref());
    }
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
        500 | 502 | 503 | 504 | 520..=524 | 529 => {
            (ErrorCategory::Unavailable, "model.openai_responses.unavailable")
        }
        400..=499 => (ErrorCategory::InvalidInput, "model.openai_responses.invalid_request"),
        _ => (ErrorCategory::Internal, "model.openai_responses.api_error"),
    };
    let mapped = AdkError::new(
        ErrorComponent::Model,
        category,
        code,
        // The API's message can echo the request.
        format!("OpenAI Responses API error: {}", adk_core::upstream_excerpt(&api_err.to_string())),
    )
    .with_provider("openai-responses")
    .with_upstream_status(status_code);
    crate::error_metadata::with_first_code(
        mapped,
        [api_err.api_error.code.as_deref(), api_err.api_error.r#type.as_deref()],
    )
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
        let long = "x".repeat(adk_core::UPSTREAM_EXCERPT_BYTES * 2);
        let message = api_error(status, long.as_bytes(), false).message;
        assert_eq!(
            message.len(),
            "HTTP 502 Bad Gateway: ".len() + adk_core::UPSTREAM_EXCERPT_BYTES
        );
    }

    #[test]
    fn bounds_an_echoed_api_message() {
        let error = ApiErrorResponse {
            status_code: reqwest_openai::StatusCode::BAD_REQUEST,
            api_error: ApiError {
                message: format!("Invalid value: '{}'", "x".repeat(4096)),
                r#type: Some("invalid_request_error".into()),
                param: None,
                code: None,
            },
        };
        let mapped = map_api_error(&error);
        assert_eq!(
            mapped.message.len(),
            "OpenAI Responses API error: ".len() + adk_core::UPSTREAM_EXCERPT_BYTES
        );
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
    fn falls_back_to_the_error_type() {
        let error = ApiErrorResponse {
            status_code: reqwest_openai::StatusCode::BAD_REQUEST,
            api_error: ApiError {
                message: "invalid".into(),
                r#type: Some("invalid_request_error".into()),
                param: None,
                code: None,
            },
        };
        let mapped = map_api_error(&error);
        assert_eq!(mapped.details.metadata["provider_error_code"], "invalid_request_error");
    }

    async fn http_error_code(body: serde_json::Value) -> Option<serde_json::Value> {
        let response = http::Response::builder().status(400).body(body.to_string()).unwrap();
        let error = normalize_response(reqwest_openai::Response::from(response), false)
            .await
            .expect_err("a 400 is an error");
        let mapped = mapped_http_error(&error).expect("an HTTP error");
        mapped.details.metadata.get("provider_error_code").cloned()
    }

    #[tokio::test]
    async fn http_errors_take_the_code_then_type_then_status() {
        assert_eq!(
            http_error_code(
                serde_json::json!({"error": {"message": "m", "type": "invalid_request_error",
                                               "param": null, "code": null}})
            )
            .await,
            Some("invalid_request_error".into())
        );
        assert_eq!(
            http_error_code(serde_json::json!({"error": {"message": "m", "code": 400,
                                               "status": "INVALID_ARGUMENT"}}))
            .await,
            Some("INVALID_ARGUMENT".into())
        );
        assert_eq!(
            http_error_code(
                serde_json::json!({"error": {"message": "m", "type": "invalid_request_error",
                                               "code": "invalid_schema"}})
            )
            .await,
            Some("invalid_schema".into())
        );
        assert_eq!(http_error_code(serde_json::json!({"error": {"message": "m"}})).await, None);
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
