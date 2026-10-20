//! Response handling helpers — internal utilities for deserializing API responses
//! and mapping HTTP errors to typed `EnterpriseError` variants.

use std::time::Duration;

use reqwest::Response;
use serde::de::DeserializeOwned;

use crate::{EnterpriseError, Result};

/// Deserialize a successful JSON response or map the error.
///
/// If the response status is success (2xx), deserializes the body as `T`.
/// Otherwise, reads the body and maps the status + body to an `EnterpriseError`.
pub(crate) async fn handle_response<T: DeserializeOwned>(response: Response) -> Result<T> {
    let status = response.status();

    if status.is_success() {
        let body = response.bytes().await.map_err(EnterpriseError::Connection)?;
        let value: T = serde_json::from_slice(&body)?;
        Ok(value)
    } else {
        let retry_after = parse_retry_after(&response);
        let body = response.text().await.unwrap_or_default();
        Err(map_api_error(status, &body, retry_after))
    }
}

/// Check that a response has a success status and return `Ok(())`, or map the error.
///
/// Used for endpoints that return empty bodies (e.g., DELETE).
pub(crate) async fn handle_empty_response(response: Response) -> Result<()> {
    let status = response.status();

    if status.is_success() {
        Ok(())
    } else {
        let retry_after = parse_retry_after(&response);
        let body = response.text().await.unwrap_or_default();
        Err(map_api_error(status, &body, retry_after))
    }
}

/// Parse the service's error envelope and map HTTP status to an `EnterpriseError` variant.
///
/// The API error envelope looks like:
/// ```json
/// {
///   "error": {
///     "type": "not_found",
///     "message": "Agent not found",
///     "param": "agent_id"
///   }
/// }
/// ```
///
/// Maps HTTP status codes to error variants:
/// - 400 → InvalidRequest
/// - 401 → Authentication
/// - 403 → Permission
/// - 404 → NotFound
/// - 409 → Conflict
/// - 422 → Validation
/// - 429 → RateLimit (with parsed Retry-After)
/// - 500 → Internal
/// - 503 → Unavailable (with parsed Retry-After)
/// - Other → Internal (generic fallback)
pub(crate) fn map_api_error(
    status: reqwest::StatusCode,
    body: &str,
    retry_after: Option<Duration>,
) -> EnterpriseError {
    // A body that is not the error envelope still yields a message.
    let (message, param) = parse_error_envelope(body);

    match status.as_u16() {
        400 => EnterpriseError::InvalidRequest {
            message: if message.is_empty() { "invalid request".into() } else { message },
            param,
        },
        401 => EnterpriseError::Authentication {
            message: if message.is_empty() { "authentication failed".into() } else { message },
        },
        403 => EnterpriseError::Permission {
            message: if message.is_empty() { "permission denied".into() } else { message },
        },
        404 => EnterpriseError::NotFound {
            message: if message.is_empty() { "not found".into() } else { message },
        },
        409 => EnterpriseError::Conflict {
            message: if message.is_empty() { "conflict".into() } else { message },
        },
        422 => EnterpriseError::Validation {
            message: if message.is_empty() { "validation error".into() } else { message },
        },
        429 => EnterpriseError::RateLimit {
            message: if message.is_empty() { "rate limited".into() } else { message },
            retry_after,
        },
        500 => EnterpriseError::Internal {
            message: if message.is_empty() { "internal server error".into() } else { message },
        },
        503 => EnterpriseError::Unavailable {
            message: if message.is_empty() { "service unavailable".into() } else { message },
            retry_after,
        },
        _ => EnterpriseError::Internal {
            message: if message.is_empty() {
                format!("unexpected status {status}")
            } else {
                message
            },
        },
    }
}

/// Attempt to parse the error envelope (`{"error": {"message", "param"}}`) from a response body.
///
/// Returns `(message, param)`. If parsing fails, returns a fallback message
/// derived from the raw body text.
fn parse_error_envelope(body: &str) -> (String, Option<String>) {
    #[derive(serde::Deserialize)]
    struct ErrorEnvelope {
        error: Option<ErrorBody>,
    }

    #[derive(serde::Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        param: Option<String>,
    }

    if let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(body)
        && let Some(error_body) = envelope.error
    {
        let message = error_body.message.unwrap_or_else(|| "unknown error".into());
        return (message, error_body.param);
    }

    // Fallback: use the raw body (trimmed) as the message.
    // Return empty string for empty bodies so callers can provide context-specific defaults.
    let trimmed = body.trim();
    (trimmed.to_string(), None)
}

/// Parse the `Retry-After` header from a response.
///
/// Supports the delay-seconds format (e.g., `Retry-After: 120`).
/// HTTP-date format is not supported and will return `None`.
fn parse_retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn test_map_api_error_400_with_envelope() {
        let body = r#"{"error":{"type":"invalid_request","message":"Missing required field","param":"name"}}"#;
        let err = map_api_error(StatusCode::BAD_REQUEST, body, None);

        match err {
            EnterpriseError::InvalidRequest { message, param } => {
                assert_eq!(message, "Missing required field");
                assert_eq!(param, Some("name".into()));
            }
            _ => panic!("expected InvalidRequest, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_401() {
        let body = r#"{"error":{"type":"authentication_error","message":"Invalid API key"}}"#;
        let err = map_api_error(StatusCode::UNAUTHORIZED, body, None);

        match err {
            EnterpriseError::Authentication { message } => {
                assert_eq!(message, "Invalid API key");
            }
            _ => panic!("expected Authentication, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_403() {
        let body = r#"{"error":{"type":"permission_error","message":"Insufficient permissions"}}"#;
        let err = map_api_error(StatusCode::FORBIDDEN, body, None);

        match err {
            EnterpriseError::Permission { message } => {
                assert_eq!(message, "Insufficient permissions");
            }
            _ => panic!("expected Permission, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_404() {
        let body =
            r#"{"error":{"type":"not_found","message":"Agent not found","param":"agent_id"}}"#;
        let err = map_api_error(StatusCode::NOT_FOUND, body, None);

        match err {
            EnterpriseError::NotFound { message } => {
                assert_eq!(message, "Agent not found");
            }
            _ => panic!("expected NotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_409() {
        let body = r#"{"error":{"type":"conflict","message":"Session already archived"}}"#;
        let err = map_api_error(StatusCode::CONFLICT, body, None);

        match err {
            EnterpriseError::Conflict { message } => {
                assert_eq!(message, "Session already archived");
            }
            _ => panic!("expected Conflict, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_422() {
        let body = r#"{"error":{"type":"validation_error","message":"Invalid model reference"}}"#;
        let err = map_api_error(StatusCode::UNPROCESSABLE_ENTITY, body, None);

        match err {
            EnterpriseError::Validation { message } => {
                assert_eq!(message, "Invalid model reference");
            }
            _ => panic!("expected Validation, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_429_with_retry_after() {
        let body = r#"{"error":{"type":"rate_limit","message":"Rate limit exceeded"}}"#;
        let retry_after = Some(Duration::from_secs(60));
        let err = map_api_error(StatusCode::TOO_MANY_REQUESTS, body, retry_after);

        match err {
            EnterpriseError::RateLimit { message, retry_after } => {
                assert_eq!(message, "Rate limit exceeded");
                assert_eq!(retry_after, Some(Duration::from_secs(60)));
            }
            _ => panic!("expected RateLimit, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_500() {
        let body = r#"{"error":{"type":"internal_error","message":"Internal server error"}}"#;
        let err = map_api_error(StatusCode::INTERNAL_SERVER_ERROR, body, None);

        match err {
            EnterpriseError::Internal { message } => {
                assert_eq!(message, "Internal server error");
            }
            _ => panic!("expected Internal, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_503_with_retry_after() {
        let body =
            r#"{"error":{"type":"unavailable","message":"Service temporarily unavailable"}}"#;
        let retry_after = Some(Duration::from_secs(30));
        let err = map_api_error(StatusCode::SERVICE_UNAVAILABLE, body, retry_after);

        match err {
            EnterpriseError::Unavailable { message, retry_after } => {
                assert_eq!(message, "Service temporarily unavailable");
                assert_eq!(retry_after, Some(Duration::from_secs(30)));
            }
            _ => panic!("expected Unavailable, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_unknown_status_with_body() {
        let body = r#"{"error":{"type":"custom","message":"Something weird happened"}}"#;
        let err = map_api_error(StatusCode::from_u16(418).unwrap(), body, None);

        match err {
            EnterpriseError::Internal { message } => {
                assert_eq!(message, "Something weird happened");
            }
            _ => panic!("expected Internal fallback, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_unknown_status_empty_body() {
        let err = map_api_error(StatusCode::from_u16(502).unwrap(), "", None);

        match err {
            EnterpriseError::Internal { message } => {
                assert_eq!(message, "unexpected status 502 Bad Gateway");
            }
            _ => panic!("expected Internal fallback, got {err:?}"),
        }
    }

    #[test]
    fn test_map_api_error_malformed_json() {
        let body = "not json at all";
        let err = map_api_error(StatusCode::BAD_REQUEST, body, None);

        match err {
            EnterpriseError::InvalidRequest { message, param } => {
                assert_eq!(message, "not json at all");
                assert_eq!(param, None);
            }
            _ => panic!("expected InvalidRequest with raw body, got {err:?}"),
        }
    }

    #[test]
    fn test_parse_error_envelope_no_error_field() {
        let body = r#"{"status":"error","detail":"something"}"#;
        let (message, param) = parse_error_envelope(body);
        // Falls back to raw body since "error" field is None
        assert_eq!(message, r#"{"status":"error","detail":"something"}"#);
        assert_eq!(param, None);
    }

    #[test]
    fn test_parse_error_envelope_missing_message() {
        let body = r#"{"error":{"type":"not_found"}}"#;
        let (message, param) = parse_error_envelope(body);
        assert_eq!(message, "unknown error");
        assert_eq!(param, None);
    }
}
