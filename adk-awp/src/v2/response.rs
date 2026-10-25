//! AWP 2.0 responses: documents as `application/awp+json` and failures as the error envelope
//! (spec 3.5 §7.5; AWP 2.0 §17).

use awp_types::v2::contract::MEDIA_TYPE;
use awp_types::{ErrorCode, ErrorEnvelope, SiteStatus};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Serializes `document` as an `application/awp+json` response with `status`.
///
/// # Example
///
/// ```
/// use adk_awp::v2::response::awp_json;
/// use axum::http::StatusCode;
///
/// let response = awp_json(StatusCode::OK, &serde_json::json!({ "ok": true }));
/// assert_eq!(response.headers()["content-type"], "application/awp+json");
/// ```
pub fn awp_json<T: Serialize>(status: StatusCode, document: &T) -> Response {
    match serde_json::to_vec(document) {
        Ok(body) => {
            let mut response = (status, body).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
            response
        }
        Err(error) => {
            tracing::error!(error = %error, "awp document failed to serialize");
            error_response(ErrorCode::InternalError, "the response could not be encoded", None)
        }
    }
}

/// Answers `envelope` with its code's HTTP status and, when it carries one, `Retry-After`.
pub fn envelope_response(envelope: &ErrorEnvelope) -> Response {
    let status = StatusCode::from_u16(envelope.error.code.http_status())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = awp_json(status, envelope);
    if let Some(seconds) = envelope.error.retry_after
        && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Answers an error envelope for `code`; `retry_after` sets `retryAfter` and `Retry-After`.
///
/// # Example
///
/// ```
/// use adk_awp::v2::response::error_response;
/// use awp_types::ErrorCode;
///
/// let response = error_response(ErrorCode::CapabilityNotFound, "no such capability", None);
/// assert_eq!(response.status(), 404);
/// ```
pub fn error_response(
    code: ErrorCode,
    message: impl Into<String>,
    retry_after: Option<u64>,
) -> Response {
    let mut envelope = ErrorEnvelope::new(code, message);
    envelope.error.retry_after = retry_after;
    envelope_response(&envelope)
}

/// Answers `503 read_only` with the signed status document (spec 3.5 §7.9; AWP 2.0 §10.3).
pub fn read_only_response(status: SiteStatus) -> Response {
    envelope_response(&ErrorEnvelope::read_only(
        "the site is read-only; only read-only capabilities answer",
        status,
    ))
}
