//! AWP middleware for version negotiation and request throttling.

use awp_types::{AwpError, AwpVersion, CURRENT_VERSION};
use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

use crate::error_response::awp_error_response;
use crate::state::AwpState;

/// Axum middleware that performs AWP version negotiation.
///
/// - Parses the `AWP-Version` request header (defaults to [`CURRENT_VERSION`] if absent)
/// - Rejects malformed versions with [`AwpError::InvalidRequest`]
/// - Returns a [`VersionMismatch`](AwpError::VersionMismatch) error if the major version differs
/// - Sets the `AWP-Version` response header to [`CURRENT_VERSION`] on success
pub async fn version_negotiation(request: Request, next: Next) -> Response {
    let version = match request.headers().get("AWP-Version") {
        None => CURRENT_VERSION,
        Some(value) => match value.to_str().ok().and_then(|raw| raw.parse::<AwpVersion>().ok()) {
            Some(version) => version,
            None => {
                return awp_error_response(AwpError::InvalidRequest(
                    "AWP-Version must be a valid major.minor version".to_string(),
                ));
            }
        },
    };

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

/// Applies the configured per-trust-level rate limit.
///
/// Authenticated identities are keyed by a one-way digest of their credential.
/// Anonymous callers use the peer IP supplied by Axum's `ConnectInfo`; when a
/// server does not provide it, all unknown anonymous callers share one
/// fail-closed bucket rather than trusting spoofable forwarding headers.
pub async fn enforce_rate_limit(
    State(state): State<AwpState>,
    request: Request,
    next: Next,
) -> Response {
    let trust_level = state.trust_assigner.assign(request.headers()).await;
    let key = rate_limit_key(&request, trust_level);

    match state.rate_limiter.check(&key, trust_level).await {
        Ok(()) => next.run(request).await,
        Err(retry_after_secs) => awp_error_response(AwpError::RateLimited { retry_after_secs }),
    }
}

fn rate_limit_key(request: &Request, trust_level: awp_types::TrustLevel) -> String {
    if trust_level != awp_types::TrustLevel::Anonymous
        && let Some(credential) = request.headers().get(axum::http::header::AUTHORIZATION)
    {
        return format!("credential:{:x}", Sha256::digest(credential.as_bytes()));
    }

    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "anonymous:unknown".to_string(), |peer| format!("ip:{}", peer.ip()))
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
    async fn test_invalid_version_header_is_rejected() {
        let app = test_app();
        let request = HttpRequest::builder()
            .uri("/test")
            .header("AWP-Version", "not-a-version")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
