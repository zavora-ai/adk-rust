//! AWP error to HTTP response conversion.
//!
//! Uses a function approach instead of `IntoResponse` impl to avoid Rust's
//! orphan rule (both `IntoResponse` and `AwpError` are foreign types).

use awp_types::{AwpError, CURRENT_VERSION};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Convert an [`AwpError`] into an Axum HTTP [`Response`].
///
/// The response includes:
/// - HTTP status code from [`AwpError::status_code()`]
/// - JSON body with `error`, `message`, and `version` fields
/// - `Retry-After` header for [`AwpError::RateLimited`] errors
///
/// # Example
///
/// ```
/// use awp_types::AwpError;
/// use adk_awp::error_response::awp_error_response;
///
/// let err = AwpError::NotFound("resource xyz".to_string());
/// let response = awp_error_response(err);
/// assert_eq!(response.status(), 404);
/// ```
pub fn awp_error_response(err: AwpError) -> Response {
    let status =
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = serde_json::json!({
        "error": err.error_code(),
        "message": err.to_string(),
        "version": CURRENT_VERSION.to_string(),
    });

    let mut response = (status, Json(body)).into_response();

    if let AwpError::RateLimited { retry_after_secs } = &err {
        if let Ok(val) = retry_after_secs.to_string().parse() {
            response.headers_mut().insert("Retry-After", val);
        }
    }

    response
}

/// Newtype wrapper for [`AwpError`] that implements [`IntoResponse`].
///
/// Use this when you need to return an `AwpError` directly from an Axum handler.
pub struct AwpErrorResponse(pub AwpError);

impl IntoResponse for AwpErrorResponse {
    fn into_response(self) -> Response {
        awp_error_response(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::AwpVersion;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn test_not_found_response() {
        let err = AwpError::NotFound("resource xyz".to_string());
        let response = awp_error_response(err);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "not_found");
        assert_eq!(json["version"], "1.0");
    }

    #[tokio::test]
    async fn test_rate_limited_response_has_retry_after() {
        let err = AwpError::RateLimited { retry_after_secs: 30 };
        let response = awp_error_response(err);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("Retry-After").unwrap(), "30");
    }

    #[tokio::test]
    async fn test_version_mismatch_response() {
        let err = AwpError::VersionMismatch {
            requested: AwpVersion { major: 2, minor: 0 },
            current: CURRENT_VERSION,
        };
        let response = awp_error_response(err);
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn test_error_response_wrapper() {
        let err = AwpErrorResponse(AwpError::Unauthorized("no token".to_string()));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_internal_error_response() {
        let err = AwpError::InternalError("something broke".to_string());
        let response = awp_error_response(err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal_error");
        assert!(json["message"].as_str().unwrap().contains("something broke"));
    }
}
