//! Auth middleware bridge for flowing authenticated identity into agent execution.
//!
//! This module defines the [`RequestContextExtractor`] trait that server operators
//! implement to extract identity from HTTP requests, and the [`RequestContextError`]
//! enum for extraction failures.
//!
//! The extracted [`RequestContext`] (re-exported from `adk-core`) carries user_id,
//! scopes, and metadata into the `InvocationContext`, making scopes available
//! to tools via `ToolContext::user_scopes()`.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::auth_bridge::{RequestContextExtractor, RequestContextError};
//! use adk_core::RequestContext;
//! use async_trait::async_trait;
//!
//! struct MyExtractor;
//!
//! #[async_trait]
//! impl RequestContextExtractor for MyExtractor {
//!     async fn extract(
//!         &self,
//!         parts: &axum::http::request::Parts,
//!     ) -> Result<RequestContext, RequestContextError> {
//!         let auth = parts.headers
//!             .get("authorization")
//!             .and_then(|v| v.to_str().ok())
//!             .ok_or(RequestContextError::MissingAuth)?;
//!         // ... validate token, build RequestContext ...
//!         # todo!()
//!     }
//! }
//! ```

pub use adk_core::RequestContext;
use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;

/// Extracts authenticated identity from HTTP request headers.
///
/// Implementations typically parse a Bearer token from the `Authorization`
/// header, validate it, and map claims to a [`RequestContext`].
#[async_trait]
pub trait RequestContextExtractor: Send + Sync {
    /// Extract identity from the request parts (headers, URI, etc.).
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError>;
}

/// Errors that can occur during request context extraction.
#[derive(Debug, thiserror::Error)]
pub enum RequestContextError {
    /// The `Authorization` header is missing from the request.
    #[error("missing authorization header")]
    MissingAuth,
    /// The token was present but failed validation.
    #[error("invalid token: {0}")]
    InvalidToken(String),
    /// An internal error occurred during extraction.
    #[error("extraction failed: {0}")]
    ExtractionFailed(String),
}

/// The authenticated principal of a request, read from the request extensions.
///
/// The server's authentication layer stores the extracted identity as an
/// `Option<RequestContext>` extension; middleware written outside this crate
/// can also store a bare [`RequestContext`]. This extractor accepts either and
/// never rejects: it yields `None` when no authentication ran, which is how a
/// handler tells an unauthenticated deployment from an authenticated caller.
///
/// # Example
///
/// ```rust
/// use adk_server::auth_bridge::AuthenticatedCaller;
///
/// async fn whoami(AuthenticatedCaller(caller): AuthenticatedCaller) -> String {
///     caller.map_or_else(|| "anonymous".to_string(), |context| context.user_id)
/// }
///
/// let _app: axum::Router = axum::Router::new().route("/whoami", axum::routing::get(whoami));
/// ```
#[derive(Debug, Clone, Default)]
pub struct AuthenticatedCaller(pub Option<RequestContext>);

impl AuthenticatedCaller {
    /// Returns the authenticated user ID, or `None` when no authentication ran.
    pub fn user_id(&self) -> Option<&str> {
        self.0.as_ref().map(|context| context.user_id.as_str())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedCaller {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let context = parts
            .extensions
            .get::<Option<RequestContext>>()
            .cloned()
            .flatten()
            .or_else(|| parts.extensions.get::<RequestContext>().cloned());
        Ok(Self(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(user_id: &str) -> RequestContext {
        RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        }
    }

    async fn extract(parts: &mut Parts) -> Option<String> {
        let caller = AuthenticatedCaller::from_request_parts(parts, &()).await.unwrap();
        caller.user_id().map(str::to_string)
    }

    #[tokio::test]
    async fn reads_the_auth_layer_extension() {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert::<Option<RequestContext>>(Some(context("alice")));
        assert_eq!(extract(&mut parts).await, Some("alice".to_string()));
    }

    #[tokio::test]
    async fn reads_a_bare_request_context_extension() {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert(context("bob"));
        assert_eq!(extract(&mut parts).await, Some("bob".to_string()));
    }

    #[tokio::test]
    async fn is_none_without_authentication() {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        assert_eq!(extract(&mut parts).await, None);
        parts.extensions.insert::<Option<RequestContext>>(None);
        assert_eq!(extract(&mut parts).await, None);
    }
}
