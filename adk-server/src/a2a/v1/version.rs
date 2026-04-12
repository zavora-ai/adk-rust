//! A2A v1.0.0 version negotiation per spec §3.6.2.
//!
//! Provides an Axum middleware that extracts the `A2A-Version` request header,
//! validates it against [`SUPPORTED_VERSIONS`], and either sets the negotiated
//! version on the response or returns a [`VersionNotSupportedError`](super::error::A2aError::VersionNotSupported).

use super::error::A2aError;
use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

/// Protocol versions this server supports.
pub const SUPPORTED_VERSIONS: &[&str] = &["0.3", "1.0"];

/// Header name for A2A protocol version negotiation.
const A2A_VERSION_HEADER: &str = "a2a-version";

/// Pure function for version negotiation logic (testable without Axum).
///
/// Returns the negotiated version string on success, or a
/// [`VersionNotSupported`](A2aError::VersionNotSupported) error when the
/// requested version is not in [`SUPPORTED_VERSIONS`].
///
/// # Rules
///
/// - `None` or empty string → defaults to `"0.3"` per spec §3.6.2.
/// - Exact match against [`SUPPORTED_VERSIONS`] → returns the matched version.
/// - Anything else → error listing supported versions.
pub fn negotiate_version(requested: Option<&str>) -> Result<&'static str, A2aError> {
    match requested {
        None | Some("") => Ok("0.3"),
        Some(v) => {
            if let Some(&supported) = SUPPORTED_VERSIONS.iter().find(|&&s| s == v) {
                Ok(supported)
            } else {
                Err(A2aError::VersionNotSupported {
                    requested: v.to_string(),
                    supported: SUPPORTED_VERSIONS.iter().map(|s| (*s).to_string()).collect(),
                })
            }
        }
    }
}

/// Axum middleware for A2A version negotiation.
///
/// Extracts the `A2A-Version` request header, validates it via
/// [`negotiate_version`], and either:
/// - Sets the `A2A-Version` response header to the negotiated version and
///   forwards the request, or
/// - Returns an HTTP 400 error with a JSON body listing supported versions.
pub async fn version_negotiation(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let requested = req.headers().get(A2A_VERSION_HEADER).and_then(|v| v.to_str().ok());

    match negotiate_version(requested) {
        Ok(version) => {
            let mut response = next.run(req).await;
            if let Ok(value) = HeaderValue::from_str(version) {
                response.headers_mut().insert(A2A_VERSION_HEADER, value);
            }
            response
        }
        Err(err) => {
            let body = err.to_http_error_response();
            (
                StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::BAD_REQUEST),
                [(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                axum::Json(body),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_version_0_3_returns_ok() {
        let result = negotiate_version(Some("0.3"));
        assert_eq!(result.unwrap(), "0.3");
    }

    #[test]
    fn supported_version_1_0_returns_ok() {
        let result = negotiate_version(Some("1.0"));
        assert_eq!(result.unwrap(), "1.0");
    }

    #[test]
    fn missing_header_defaults_to_0_3() {
        let result = negotiate_version(None);
        assert_eq!(result.unwrap(), "0.3");
    }

    #[test]
    fn empty_header_defaults_to_0_3() {
        let result = negotiate_version(Some(""));
        assert_eq!(result.unwrap(), "0.3");
    }

    #[test]
    fn unsupported_version_returns_error_with_supported_list() {
        let result = negotiate_version(Some("2.0"));
        let err = result.unwrap_err();
        match &err {
            A2aError::VersionNotSupported { requested, supported } => {
                assert_eq!(requested, "2.0");
                assert_eq!(supported, &["0.3", "1.0"]);
            }
            other => panic!("expected VersionNotSupported, got: {other}"),
        }
        assert_eq!(err.json_rpc_code(), -32009);
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn unsupported_version_0_1_returns_error() {
        let result = negotiate_version(Some("0.1"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            A2aError::VersionNotSupported { requested, .. } => {
                assert_eq!(requested, "0.1");
            }
            other => panic!("expected VersionNotSupported, got: {other}"),
        }
    }

    #[test]
    fn unsupported_version_garbage_returns_error() {
        let result = negotiate_version(Some("not-a-version"));
        assert!(result.is_err());
    }

    #[test]
    fn all_supported_versions_return_ok() {
        for &version in SUPPORTED_VERSIONS {
            let result = negotiate_version(Some(version));
            assert_eq!(result.unwrap(), version);
        }
    }
}
