//! MCP callers from the server's own identity layer.

use std::sync::Arc;

use adk_core::request_context::RequestContextExtractor;
use adk_core::{AdkError, ErrorComponent, RequestContext, Result};
use adk_tool::mcp::server::{McpCaller, McpCallerResolver, McpHttpRequest};
use async_trait::async_trait;

/// Resolves each MCP request's caller with the server's [`RequestContextExtractor`], the same
/// token and identity model as the REST routes: the caller is authenticated as the context's
/// `user_id`, and carries the [`RequestContext`] as an extension.
///
/// A `RequestContext` an earlier layer already attached to the request is used as is. A request
/// the extractor refuses is answered `401` with `WWW-Authenticate: Bearer`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::RequestContext;
/// use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
/// use adk_server::mcp::RequestContextCallers;
/// use adk_tool::mcp::server::{McpCallerResolver, McpHttpRequest};
///
/// struct OneToken;
///
/// #[async_trait::async_trait]
/// impl RequestContextExtractor for OneToken {
///     async fn extract(
///         &self,
///         parts: &axum::http::request::Parts,
///     ) -> Result<RequestContext, RequestContextError> {
///         match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
///             Some("Bearer ana-token") => Ok(RequestContext {
///                 user_id: "ana".to_string(),
///                 scopes: vec!["orders".to_string()],
///                 metadata: Default::default(),
///             }),
///             _ => Err(RequestContextError::MissingAuth),
///         }
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let callers = RequestContextCallers::new(Arc::new(OneToken));
/// let (parts, _) = axum::http::Request::builder()
///     .header("Authorization", "Bearer ana-token")
///     .body(())
///     .unwrap()
///     .into_parts();
/// let caller = callers.resolve(&McpHttpRequest::new(&parts)).await.unwrap();
/// assert_eq!(caller.id(), "ana");
/// assert_eq!(caller.extension::<RequestContext>().unwrap().scopes, ["orders"]);
/// # });
/// ```
#[derive(Clone)]
pub struct RequestContextCallers {
    extractor: Arc<dyn RequestContextExtractor>,
}

impl std::fmt::Debug for RequestContextCallers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContextCallers").finish_non_exhaustive()
    }
}

impl RequestContextCallers {
    /// Resolves callers with `extractor`.
    pub fn new(extractor: Arc<dyn RequestContextExtractor>) -> Self {
        Self { extractor }
    }
}

#[async_trait]
impl McpCallerResolver for RequestContextCallers {
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller> {
        let attached =
            request.extensions().get::<RequestContext>().cloned().or_else(|| {
                request.extensions().get::<Option<RequestContext>>().cloned().flatten()
            });
        let context = match attached {
            Some(context) => context,
            None => self.extractor.extract(request.parts()).await.map_err(|error| {
                AdkError::unauthorized(
                    ErrorComponent::Auth,
                    "mcp.caller.unauthenticated",
                    format!("the request carries no identity this server accepts: {error}"),
                )
            })?,
        };
        Ok(McpCaller::authenticated(context.user_id.clone()).with_extension(context))
    }
}
