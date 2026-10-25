//! AWP middleware for version negotiation and request throttling.

use awp_types::{AwpError, AwpVersion, CURRENT_VERSION};
use axum::extract::Request;
#[cfg(feature = "awp-v1")]
use axum::extract::{ConnectInfo, State};
use axum::middleware::Next;
use axum::response::Response;
#[cfg(feature = "awp-v1")]
use sha2::{Digest, Sha256};
#[cfg(feature = "awp-v1")]
use std::net::SocketAddr;

use crate::error_response::awp_error_response;
#[cfg(feature = "awp-v1")]
use crate::state::AwpState;
use crate::v2::negotiation::{NegotiationError, parse_offered, select};

/// Axum middleware that performs AWP version negotiation for the 1.x routes.
///
/// - Parses the `AWP-Version` request header, a list such as `2.0, 1.0` (spec 3.5 §7.2);
///   defaults to [`CURRENT_VERSION`] if absent
/// - Rejects malformed versions with [`AwpError::InvalidRequest`]
/// - Returns a [`VersionMismatch`](AwpError::VersionMismatch) error if no offered major is 1
/// - Sets the `AWP-Version` response header to [`CURRENT_VERSION`] on success
pub async fn version_negotiation(request: Request, next: Next) -> Response {
    if let Some(value) = request.headers().get("AWP-Version") {
        let Some(offered) = value.to_str().ok().and_then(|raw| parse_offered(raw).ok()) else {
            return awp_error_response(AwpError::InvalidRequest(
                "AWP-Version must list major.minor versions, highest first".to_string(),
            ));
        };
        if let Err(NegotiationError::NoCommonMajor { .. }) =
            select(Some(&offered), &[CURRENT_VERSION])
        {
            let requested = offered.first().copied().unwrap_or(AwpVersion { major: 0, minor: 0 });
            return awp_error_response(AwpError::VersionMismatch {
                requested,
                current: CURRENT_VERSION,
            });
        }
    }

    let mut response = next.run(request).await;
    if let Ok(val) = CURRENT_VERSION.to_string().parse() {
        response.headers_mut().insert("AWP-Version", val);
    }
    response
}

/// Applies the configured per-trust-level rate limit.
#[cfg(feature = "awp-v1")]
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

#[cfg(feature = "awp-v1")]
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

    /// A 2.0 client that also speaks 1.x is answered in 1.0 (spec 3.5 §7.2).
    #[tokio::test]
    async fn test_version_list_selects_the_common_major() {
        let request = HttpRequest::builder()
            .uri("/test")
            .header("AWP-Version", "2.0, 1.2")
            .body(Body::empty())
            .unwrap();
        let response = test_app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["AWP-Version"], "1.0");
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
