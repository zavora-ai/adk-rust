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
