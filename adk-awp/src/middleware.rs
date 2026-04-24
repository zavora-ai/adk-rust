//! AWP middleware for version negotiation.

use awp_types::{AwpError, AwpVersion, CURRENT_VERSION};
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::error_response::awp_error_response;

/// Axum middleware that performs AWP version negotiation.
///
/// - Parses the `AWP-Version` request header (defaults to [`CURRENT_VERSION`] if absent)
/// - Returns a [`VersionMismatch`](AwpError::VersionMismatch) error if the major version differs
/// - Sets the `AWP-Version` response header to [`CURRENT_VERSION`] on success
pub async fn version_negotiation(request: Request, next: Next) -> Response {
    let version = request
        .headers()
        .get("AWP-Version")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<AwpVersion>().ok())
        .unwrap_or(CURRENT_VERSION);

    if !CURRENT_VERSION.is_compatible(&version) {
        return awp_error_response(AwpError::VersionMismatch {
            requested: version,
            current: CURRENT_VERSION,
        });
    }

    let mut response = next.run(request).await;
    if let Ok(val) = CURRENT_VERSION.to_string().parse() {
        response.headers_mut().insert("AWP-Version", val);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::middleware::from_fn;
    use axum::routing::get;
    use tower::util::ServiceExt;

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn test_app() -> Router {
        Router::new().route("/test", get(ok_handler)).layer(from_fn(version_negotiation))
    }

    #[tokio::test]
    async fn test_no_version_header_defaults() {
        let app = test_app();
        let request = HttpRequest::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("AWP-Version").unwrap().to_str().unwrap(), "1.0");
    }

    #[tokio::test]
    async fn test_compatible_version_accepted() {
        let app = test_app();
        let request = HttpRequest::builder()
            .uri("/test")
            .header("AWP-Version", "1.1")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("AWP-Version").unwrap().to_str().unwrap(), "1.0");
    }

    #[tokio::test]
    async fn test_incompatible_version_rejected() {
        let app = test_app();
        let request = HttpRequest::builder()
            .uri("/test")
            .header("AWP-Version", "2.0")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn test_invalid_version_header_defaults() {
        let app = test_app();
        let request = HttpRequest::builder()
            .uri("/test")
            .header("AWP-Version", "not-a-version")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Invalid version string can't be parsed, defaults to CURRENT_VERSION
        assert_eq!(response.status(), StatusCode::OK);
    }
}
