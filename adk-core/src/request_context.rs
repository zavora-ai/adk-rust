use std::collections::HashMap;

/// Identity and authorization context extracted from an HTTP request.
///
/// This struct carries authenticated identity fields (user_id, scopes, metadata)
/// that flow from the server's auth middleware into the agent invocation context.
/// It lives in `adk-core` so that both `adk-server` (which produces it) and
/// `adk-runner` (which consumes it) can reference it without circular dependencies.
///
/// # Example
///
/// ```rust
/// use adk_core::RequestContext;
///
/// let ctx = RequestContext {
///     user_id: "user-123".to_string(),
///     scopes: vec!["read".to_string(), "write".to_string()],
///     metadata: [("tenant".to_string(), "acme".to_string())].into(),
/// };
/// assert_eq!(ctx.user_id, "user-123");
/// assert_eq!(ctx.scopes.len(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// Authenticated user ID (e.g. from JWT `sub` or `email` claim).
    pub user_id: String,
    /// Granted scopes (e.g. from JWT `scope` or `scp` claim).
    pub scopes: Vec<String>,
    /// Additional metadata for custom middleware use.
    pub metadata: HashMap<String, String>,
}

/// Extracts authenticated identity from HTTP request headers.
///
/// Implementations typically parse a Bearer token from the `Authorization`
/// header, validate it, and map claims to a [`RequestContext`]. `adk-server`
/// re-exports it as `adk_server::auth_bridge::RequestContextExtractor`.
///
/// # Example
///
/// ```rust
/// use adk_core::RequestContext;
/// use adk_core::request_context::{RequestContextError, RequestContextExtractor};
///
/// struct Header;
///
/// #[async_trait::async_trait]
/// impl RequestContextExtractor for Header {
///     async fn extract(
///         &self,
///         parts: &http::request::Parts,
///     ) -> Result<RequestContext, RequestContextError> {
///         let user = parts
///             .headers
///             .get("x-user")
///             .and_then(|value| value.to_str().ok())
///             .ok_or(RequestContextError::MissingAuth)?;
///         Ok(RequestContext { user_id: user.to_string(), scopes: vec![], metadata: Default::default() })
///     }
/// }
/// ```
#[cfg(feature = "auth-bridge")]
#[async_trait::async_trait]
pub trait RequestContextExtractor: Send + Sync {
    /// Extract identity from the request parts (headers, URI, etc.).
    async fn extract(
        &self,
        parts: &http::request::Parts,
    ) -> Result<RequestContext, RequestContextError>;
}

/// Errors that can occur during request context extraction.
#[cfg(feature = "auth-bridge")]
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
