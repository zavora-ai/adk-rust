//! Request identity for a served harness: loopback operators and bearer tokens
//! (spec 3.1 §8.2).

use std::collections::HashMap;
use std::net::SocketAddr;

use adk_core::RequestContext;
use adk_server::{RequestContextError, RequestContextExtractor};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use sha2::{Digest, Sha256};

/// The scope a served harness grants its operator; the approval inbox routes held calls to
/// this group.
pub const OPERATOR_SCOPE: &str = "operator";

/// Header the loopback layer sets on requests from a loopback peer.
///
/// The server's run routes hand extractors a request rebuilt from its headers, without
/// the connection's address, so the layer records the peer check in a header carrying a
/// per-process secret that a client cannot forge.
const LOOPBACK_HEADER: &str = "x-adk-loopback-peer";

/// The operator's request context: `user_id` with the [`OPERATOR_SCOPE`].
pub(crate) fn operator(user_id: &str) -> RequestContext {
    RequestContext {
        user_id: user_id.to_string(),
        scopes: vec![OPERATOR_SCOPE.to_string()],
        metadata: HashMap::new(),
    }
}

/// Admits requests from loopback peers as the operator: `RequestContext { user_id, scopes:
/// ["operator"] }` (spec 3.1 §8.2).
///
/// It reads the peer address from `ConnectInfo<SocketAddr>`, so serve the router with
/// `into_make_service_with_connect_info::<SocketAddr>()`, and apply [`layer`](Self::layer)
/// to it so routes that see only request headers can tell a loopback peer too. A request
/// from any other peer, or with no peer address, is refused as unauthenticated.
///
/// # Example
///
/// ```rust
/// use std::net::SocketAddr;
///
/// use adk_cli::harness::LoopbackOperatorExtractor;
/// use adk_server::RequestContextExtractor;
/// use axum::extract::ConnectInfo;
///
/// # futures::executor::block_on(async {
/// let extractor = LoopbackOperatorExtractor::new("ana");
/// let mut request = axum::http::Request::builder().body(()).unwrap();
/// request.extensions_mut().insert(ConnectInfo("127.0.0.1:50000".parse::<SocketAddr>().unwrap()));
/// let (parts, _) = request.into_parts();
/// let context = extractor.extract(&parts).await.unwrap();
/// assert_eq!((context.user_id.as_str(), context.scopes.as_slice()), ("ana", &["operator".to_string()][..]));
/// # });
/// ```
#[derive(Clone)]
pub struct LoopbackOperatorExtractor {
    user_id: String,
    secret: String,
}

impl std::fmt::Debug for LoopbackOperatorExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackOperatorExtractor").field("user_id", &self.user_id).finish()
    }
}

impl LoopbackOperatorExtractor {
    /// Admits loopback peers as `user_id`, the operator running the server.
    pub fn new(user_id: impl Into<String>) -> Self {
        Self { user_id: user_id.into(), secret: uuid::Uuid::new_v4().simple().to_string() }
    }

    /// Records, on every request, whether its peer is on loopback, for routes that see
    /// only headers. A header of the same name sent by a client is removed first.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::harness::LoopbackOperatorExtractor;
    ///
    /// let extractor = LoopbackOperatorExtractor::new("ana");
    /// let router: axum::Router = extractor.layer(axum::Router::new());
    /// # let _ = router;
    /// ```
    pub fn layer(&self, router: Router) -> Router {
        let secret = self.secret.clone();
        router.layer(middleware::from_fn(move |mut request: Request<Body>, next: Next| {
            let secret = secret.clone();
            async move {
                request.headers_mut().remove(LOOPBACK_HEADER);
                let loopback = request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
                if loopback && let Ok(value) = HeaderValue::from_str(&secret) {
                    request.headers_mut().insert(LOOPBACK_HEADER, value);
                }
                let response: Response = next.run(request).await;
                response
            }
        }))
    }
}

#[async_trait]
impl RequestContextExtractor for LoopbackOperatorExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|ConnectInfo(peer)| *peer);
        let admitted = match peer {
            Some(peer) => peer.ip().is_loopback(),
            None => parts
                .headers
                .get(LOOPBACK_HEADER)
                .is_some_and(|value| constant_time_eq(value.as_bytes(), self.secret.as_bytes())),
        };
        if admitted { Ok(operator(&self.user_id)) } else { Err(RequestContextError::MissingAuth) }
    }
}

/// Admits requests carrying `Authorization: Bearer <token>` as the operator (spec 3.1
/// §8.2).
///
/// The token comes from `--auth-token` or `ADK_SERVE_TOKEN` and is compared in constant
/// time. A served harness on a non-loopback address requires it.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::BearerTokenExtractor;
/// use adk_server::RequestContextExtractor;
///
/// # futures::executor::block_on(async {
/// let extractor = BearerTokenExtractor::new("s3cret-token", "ana");
/// let request = axum::http::Request::builder()
///     .header("authorization", "Bearer s3cret-token")
///     .body(())
///     .unwrap();
/// let (parts, _) = request.into_parts();
/// assert_eq!(extractor.extract(&parts).await.unwrap().user_id, "ana");
/// # });
/// ```
#[derive(Clone)]
pub struct BearerTokenExtractor {
    digest: [u8; 32],
    user_id: String,
}

impl std::fmt::Debug for BearerTokenExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BearerTokenExtractor").field("user_id", &self.user_id).finish()
    }
}

impl BearerTokenExtractor {
    /// Admits holders of `token` as `user_id`.
    pub fn new(token: impl AsRef<str>, user_id: impl Into<String>) -> Self {
        Self { digest: Sha256::digest(token.as_ref().as_bytes()).into(), user_id: user_id.into() }
    }
}

#[async_trait]
impl RequestContextExtractor for BearerTokenExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .ok_or(RequestContextError::MissingAuth)?;
        let token =
            header.to_str().ok().and_then(|value| value.strip_prefix("Bearer ")).ok_or_else(
                || RequestContextError::InvalidToken("expected a bearer token".into()),
            )?;
        // Comparing digests keeps the time independent of the token's length and content.
        let presented: [u8; 32] = Sha256::digest(token.trim().as_bytes()).into();
        if constant_time_eq(&presented, &self.digest) {
            Ok(operator(&self.user_id))
        } else {
            Err(RequestContextError::InvalidToken("the token does not match".into()))
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(peer: Option<&str>, headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut builder = Request::builder();
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = builder.body(()).unwrap();
        if let Some(peer) = peer {
            request.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        request.into_parts().0
    }

    #[tokio::test]
    async fn loopback_peers_are_the_operator_and_others_are_refused() {
        let extractor = LoopbackOperatorExtractor::new("ana");
        for peer in ["127.0.0.1:1", "[::1]:1"] {
            let context = extractor.extract(&parts(Some(peer), &[])).await.unwrap();
            assert_eq!(context.user_id, "ana");
            assert_eq!(context.scopes, vec![OPERATOR_SCOPE.to_string()]);
        }
        for request in [parts(Some("10.0.0.7:1"), &[]), parts(None, &[])] {
            assert!(matches!(
                extractor.extract(&request).await,
                Err(RequestContextError::MissingAuth)
            ));
        }
    }

    #[tokio::test]
    async fn a_forged_loopback_header_is_refused() {
        let extractor = LoopbackOperatorExtractor::new("ana");
        let forged = parts(None, &[(LOOPBACK_HEADER, "guess")]);
        assert!(extractor.extract(&forged).await.is_err());
        let genuine = parts(None, &[(LOOPBACK_HEADER, &extractor.secret)]);
        assert_eq!(extractor.extract(&genuine).await.unwrap().user_id, "ana");
        // A remote peer with the right header is still refused: the address decides.
        let remote = parts(Some("10.0.0.7:1"), &[(LOOPBACK_HEADER, &extractor.secret)]);
        assert!(extractor.extract(&remote).await.is_err());
    }

    #[tokio::test]
    async fn bearer_tokens_must_match() {
        let extractor = BearerTokenExtractor::new("right", "ana");
        assert!(matches!(
            extractor.extract(&parts(None, &[])).await,
            Err(RequestContextError::MissingAuth)
        ));
        for header in ["Bearer wrong", "Basic right", "Bearer righ", "Bearer rightt"] {
            assert!(
                extractor.extract(&parts(None, &[("authorization", header)])).await.is_err(),
                "{header}"
            );
        }
        let context =
            extractor.extract(&parts(None, &[("authorization", "Bearer right")])).await.unwrap();
        assert_eq!(context.user_id, "ana");
    }
}
