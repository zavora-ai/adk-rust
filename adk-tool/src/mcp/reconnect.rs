// MCP Connection Refresher
//
// Provides automatic reconnection for MCP connections when they fail.
// Based on adk-go's connectionRefresher pattern.
//
// Handles:
// - Connection closed errors, including rmcp's `Transport closed`
// - EOF errors
// - Session not found errors
// - HTTP 401 rejections, so a factory can reconnect with fresh credentials
// - Automatic retry with reconnection

use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResponse, Tool as McpTool},
    service::RunningService,
};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// Errors that should trigger a connection refresh
pub fn should_refresh_connection(error: &str) -> bool {
    let error_lower = error.to_lowercase();

    // Connection closed
    if error_lower.contains("connection closed") || error_lower.contains("connectionclosed") {
        return true;
    }

    // EOF / pipe closed
    if error_lower.contains("eof")
        || error_lower.contains("closed pipe")
        || error_lower.contains("broken pipe")
    {
        return true;
    }

    // Session not found (server restarted)
    if error_lower.contains("session not found") || error_lower.contains("session missing") {
        return true;
    }

    // Transport errors. rmcp reports `Transport closed` for every request pending or
    // sent after the transport stops, as it does when a stdio server process exits.
    if error_lower.contains("transport error")
        || error_lower.contains("transport closed")
        || error_lower.contains("connection reset")
    {
        return true;
    }

    is_auth_rejection(error)
}

/// Returns `true` when the transport reports that the server rejected the
/// request's credentials with HTTP 401.
///
/// rmcp reports a 401 that carries `WWW-Authenticate` as `Auth required`, and
/// one without it as `HTTP 401 Unauthorized` or reqwest's status error. All of
/// them arrive as transport errors, so a JSON-RPC error whose message merely
/// mentions authorization is not matched: that request reached the server.
pub(crate) fn is_auth_rejection(error: &str) -> bool {
    let error_lower = error.to_lowercase();
    error_lower.starts_with("transport")
        && (error_lower.contains("auth required") || error_lower.contains("401 unauthorized"))
}

/// Replay of `tools/call` after an ambiguous connection failure is opt-in.
///
/// A transport failure after the request was transmitted does not prove the call did not run.
/// Flipping this to `true` silently re-enables duplicate external writes — a payment, message,
/// or deletion executing more than once. See #504.
pub(crate) const DEFAULT_RETRY_TOOL_CALLS: bool = false;

/// A request rejected with HTTP 401 never ran, so it is retried after a
/// reconnect even when replay is not allowed.
pub(crate) fn should_retry_mcp_operation(
    error: &str,
    attempt: u32,
    refresh_config: &RefreshConfig,
    has_connection_factory: bool,
    replay_allowed: bool,
) -> bool {
    (replay_allowed || is_auth_rejection(error))
        && has_connection_factory
        && attempt < refresh_config.max_attempts
        && should_refresh_connection(error)
}

/// Result of an operation with retry information
#[derive(Debug, Clone)]
pub struct RetryResult<T> {
    /// The result value
    pub value: T,
    /// Whether a reconnection occurred
    pub reconnected: bool,
}

impl<T> RetryResult<T> {
    /// Create a new result without reconnection
    pub fn ok(value: T) -> Self {
        Self { value, reconnected: false }
    }

    /// Create a new result after reconnection
    pub fn reconnected(value: T) -> Self {
        Self { value, reconnected: true }
    }
}

/// Configuration for connection refresh behavior
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    /// Maximum number of reconnection attempts
    pub max_attempts: u32,
    /// Delay between reconnection attempts in milliseconds
    pub retry_delay_ms: u64,
    /// Whether to log reconnection attempts
    pub log_reconnections: bool,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self { max_attempts: 3, retry_delay_ms: 1000, log_reconnections: true }
    }
}

impl RefreshConfig {
    /// Create a new config with custom max attempts
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Create a new config with custom retry delay
    pub fn with_retry_delay_ms(mut self, delay_ms: u64) -> Self {
        self.retry_delay_ms = delay_ms;
        self
    }

    /// Disable logging
    pub fn without_logging(mut self) -> Self {
        self.log_reconnections = false;
        self
    }
}

/// Factory trait for creating new MCP connections.
///
/// Implement this trait to provide reconnection capability to `ConnectionRefresher`.
#[async_trait::async_trait]
pub trait ConnectionFactory<S>: Send + Sync
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new connection to the MCP server.
    async fn create_connection(&self) -> Result<RunningService<RoleClient, S>, String>;
}

/// Connection refresher that wraps an MCP client and handles automatic reconnection.
///
/// This is similar to adk-go's `connectionRefresher` struct. It transparently
/// retries operations after reconnecting when the underlying session fails.
///
/// Requests share the current connection and run concurrently: each one holds
/// the connection only for its own lifetime, never a lock. A connection that
/// has already closed, such as one to a stdio server process that exited, is
/// replaced before the next request is sent, so that request is never a replay.
/// Concurrent failures on one connection reconnect once.
///
/// # Type Parameters
///
/// * `S` - The service type for the MCP client
/// * `F` - The factory type for creating new connections
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::mcp::{ConnectionRefresher, ConnectionFactory};
///
/// struct MyFactory { /* ... */ }
///
/// #[async_trait::async_trait]
/// impl ConnectionFactory<MyService> for MyFactory {
///     async fn create_connection(&self) -> Result<RunningService<RoleClient, MyService>, String> {
///         // Create and return a new connection
///     }
/// }
///
/// let refresher = ConnectionRefresher::new(initial_client, Arc::new(factory));
///
/// // Read-only discovery automatically retries on connection failure.
/// let tools = refresher.list_tools().await?;
///
/// // Tool calls are not replayed unless the caller explicitly opts in.
/// // The server chooses whether to answer inline or with a task, so the
/// // caller reads the response to find out which happened.
/// let response = refresher
///     .with_tool_call_retries()
///     .call_tool(params)
///     .await?
///     .value;
/// match response {
///     CallToolResponse::Complete(result) => println!("{result:?}"),
///     other => println!("not a plain result: {other:?}"),
/// }
/// ```
pub struct ConnectionRefresher<S, F>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
    F: ConnectionFactory<S>,
{
    /// The current connection. Requests clone the `Arc` and release the lock before sending.
    client: RwLock<Option<Arc<RunningService<RoleClient, S>>>>,
    /// Held while a connection is created, so concurrent failures replace a dead
    /// connection once instead of once per caller.
    connecting: Mutex<()>,
    /// Factory for creating new connections
    factory: Arc<F>,
    /// Configuration for refresh behavior
    config: RefreshConfig,
    /// Whether ambiguous tool-call outcomes may be replayed after reconnection.
    retry_tool_calls: bool,
}

impl<S, F> ConnectionRefresher<S, F>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
    F: ConnectionFactory<S>,
{
    /// Create a new connection refresher with an initial client and factory.
    ///
    /// # Arguments
    ///
    /// * `client` - The initial MCP client connection
    /// * `factory` - Factory for creating new connections when needed
    pub fn new(client: RunningService<RoleClient, S>, factory: Arc<F>) -> Self {
        Self {
            client: RwLock::new(Some(Arc::new(client))),
            connecting: Mutex::new(()),
            factory,
            config: RefreshConfig::default(),
            retry_tool_calls: DEFAULT_RETRY_TOOL_CALLS,
        }
    }

    /// Create a new connection refresher without an initial connection.
    ///
    /// The first operation will trigger a connection.
    pub fn lazy(factory: Arc<F>) -> Self {
        Self {
            client: RwLock::new(None),
            connecting: Mutex::new(()),
            factory,
            config: RefreshConfig::default(),
            retry_tool_calls: DEFAULT_RETRY_TOOL_CALLS,
        }
    }

    /// Set the refresh configuration.
    pub fn with_config(mut self, config: RefreshConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the maximum number of reconnection attempts.
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.config.max_attempts = attempts;
        self
    }

    /// Allow `tools/call` to be replayed after reconnecting.
    ///
    /// A transport failure after request transmission is an ambiguous outcome:
    /// a mutating tool may have completed its external effect before the
    /// response was lost. Enable this only for read-only tools or operations
    /// protected by a stable provider idempotency guarantee.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let refresher = ConnectionRefresher::new(client, Arc::new(factory))
    ///     .with_tool_call_retries();
    /// ```
    pub fn with_tool_call_retries(mut self) -> Self {
        self.retry_tool_calls = true;
        self
    }

    /// Returns a connection to send on: the current one, or a new one when there
    /// is none or the current one has closed.
    async fn connection(&self) -> Result<Arc<RunningService<RoleClient, S>>, String> {
        if let Some(client) = self.client.read().await.as_ref()
            && !is_connection_closed(client)
        {
            return Ok(Arc::clone(client));
        }
        self.replace(None).await
    }

    /// Installs a new connection unless another caller already replaced `failed`
    /// with a live one, and returns the connection to use.
    ///
    /// With `failed` set to `None`, only a missing or closed connection is replaced.
    async fn replace(
        &self,
        failed: Option<&Arc<RunningService<RoleClient, S>>>,
    ) -> Result<Arc<RunningService<RoleClient, S>>, String> {
        let _connecting = self.connecting.lock().await;
        if let Some(current) = self.client.read().await.as_ref()
            && !is_connection_closed(current)
            && failed.is_none_or(|failed| !Arc::ptr_eq(current, failed))
        {
            return Ok(Arc::clone(current));
        }

        if self.config.log_reconnections {
            info!("creating MCP connection");
        }
        // The old connection closes once the last in-flight request on it drops its `Arc`.
        let created = Arc::new(self.factory.create_connection().await?);
        *self.client.write().await = Some(Arc::clone(&created));
        Ok(created)
    }

    /// Sends one request through `send`, reconnecting and resending after a
    /// retryable connection failure when `replay_allowed` permits it.
    async fn with_reconnect<T, Fut>(
        &self,
        operation: &str,
        replay_allowed: bool,
        mut send: impl FnMut(Arc<RunningService<RoleClient, S>>) -> Fut,
    ) -> Result<RetryResult<T>, String>
    where
        Fut: std::future::Future<Output = Result<T, rmcp::ServiceError>>,
    {
        let mut connection = self.connection().await?;
        let mut attempt = 0u32;
        loop {
            let error = match send(Arc::clone(&connection)).await {
                Ok(value) => return Ok(RetryResult { value, reconnected: attempt > 0 }),
                Err(error) => error.to_string(),
            };
            if !should_retry_mcp_operation(&error, attempt, &self.config, true, replay_allowed) {
                if !replay_allowed
                    && should_refresh_connection(&error)
                    && !is_auth_rejection(&error)
                {
                    return Err(format!(
                        "MCP {operation} result is uncertain and was not replayed: {error}. \
                         Enable tool-call retries only for replay-safe operations"
                    ));
                }
                return Err(error);
            }
            if self.config.log_reconnections {
                warn!(error = %error, operation, "MCP request failed, will retry with reconnection");
            }

            // A failed reconnect counts as an attempt, like a failed request.
            loop {
                attempt += 1;
                if self.config.retry_delay_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        self.config.retry_delay_ms,
                    ))
                    .await;
                }
                match self.replace(Some(&connection)).await {
                    Ok(replacement) => {
                        if self.config.log_reconnections {
                            debug!(attempt, operation, "reconnected MCP connection");
                        }
                        connection = replacement;
                        break;
                    }
                    Err(refresh_error) if attempt < self.config.max_attempts => {
                        if self.config.log_reconnections {
                            warn!(error = %refresh_error, attempt, "MCP reconnection failed");
                        }
                    }
                    Err(refresh_error) => {
                        return Err(format!("{error}; reconnecting failed: {refresh_error}"));
                    }
                }
            }
        }
    }

    /// List all tools from the MCP server with automatic reconnection.
    ///
    /// Handles pagination internally and restarts from scratch if
    /// reconnection occurs (per MCP spec, cursors don't persist across sessions).
    pub async fn list_tools(&self) -> Result<RetryResult<Vec<McpTool>>, String> {
        self.with_reconnect(
            "list_tools",
            true,
            |client| async move { client.list_all_tools().await },
        )
        .await
    }

    /// Call a tool on the MCP server.
    ///
    /// Tool calls are not replayed by default after an ambiguous connection
    /// failure. Use [`Self::with_tool_call_retries`] only when the operation is
    /// known to be replay-safe. A call rejected with HTTP 401 never ran, so it is
    /// resent after a reconnect either way.
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<RetryResult<CallToolResponse>, String> {
        self.with_reconnect("tool call", self.retry_tool_calls, |client| {
            let params = params.clone();
            async move { client.call_tool_once(params).await }
        })
        .await
    }

    /// Get the cancellation token for the current connection.
    pub async fn cancellation_token(
        &self,
    ) -> Option<rmcp::service::RunningServiceCancellationToken> {
        self.client.read().await.as_ref().map(|client| client.cancellation_token())
    }

    /// Check if currently connected.
    pub async fn is_connected(&self) -> bool {
        self.client.read().await.is_some()
    }

    /// Force a reconnection.
    ///
    /// Requests already in flight finish on the old connection.
    pub async fn reconnect(&self) -> Result<(), String> {
        let current = self.client.read().await.clone();
        match current {
            Some(current) => self.replace(Some(&current)).await.map(drop),
            None => self.replace(None).await.map(drop),
        }
    }

    /// Close the connection.
    pub async fn close(&self) {
        if let Some(client) = self.client.write().await.take() {
            client.cancellation_token().cancel();
        }
    }
}

/// Returns `true` when `client` can no longer carry requests: its service loop was
/// cancelled, or its transport closed, as it does when a stdio server process exits.
pub(crate) fn is_connection_closed<S>(client: &RunningService<RoleClient, S>) -> bool
where
    S: rmcp::service::Service<RoleClient>,
{
    client.is_closed() || client.peer().is_transport_closed()
}

/// Simple wrapper for MCP clients that don't support reconnection.
///
/// Use this for stdio-based MCP servers where reconnection isn't possible
/// without restarting the server process. Requests share the connection and
/// run concurrently.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::mcp::{SimpleClient, rmcp::{ServiceExt, transport::TokioChildProcess}};
/// use tokio::process::Command;
///
/// let client = ().serve(TokioChildProcess::new(Command::new("my-mcp-server"))?).await?;
/// let simple = SimpleClient::new(client);
/// let (tools, peer_info) = tokio::join!(simple.list_tools(), async {
///     simple.inner().peer_info()
/// });
/// ```
pub struct SimpleClient<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    client: Arc<RunningService<RoleClient, S>>,
}

impl<S> SimpleClient<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new simple client wrapper.
    pub fn new(client: RunningService<RoleClient, S>) -> Self {
        Self { client: Arc::new(client) }
    }

    /// List all tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, String> {
        self.client.list_all_tools().await.map_err(|e| e.to_string())
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResponse, String> {
        self.client.call_tool_once(params).await.map_err(|e| e.to_string())
    }

    /// Get the cancellation token.
    pub async fn cancellation_token(&self) -> rmcp::service::RunningServiceCancellationToken {
        self.client.cancellation_token()
    }

    /// Get the shared connection.
    ///
    /// Clone the `Arc` to send requests directly; nothing serializes them.
    pub fn inner(&self) -> &Arc<RunningService<RoleClient, S>> {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_refresh_connection() {
        assert!(should_refresh_connection("connection closed"));
        assert!(should_refresh_connection("ConnectionClosed"));
        assert!(should_refresh_connection("EOF"));
        assert!(should_refresh_connection("eof error"));
        assert!(should_refresh_connection("broken pipe"));
        assert!(should_refresh_connection("session not found"));
        assert!(should_refresh_connection("transport error"));
        assert!(should_refresh_connection("connection reset"));
        // rmcp's `ServiceError::TransportClosed`, reported once a stdio server exits.
        assert!(should_refresh_connection("Transport closed"));

        // Should not refresh for other errors
        assert!(!should_refresh_connection("invalid argument"));
        assert!(!should_refresh_connection("permission denied"));
        assert!(!should_refresh_connection("tool not found"));
    }

    #[test]
    fn http_401_rejections_trigger_a_refresh() {
        let with_challenge = "Transport send error: Transport [rmcp::transport::\
            StreamableHttpClientTransport] error: Auth required";
        let post_without_challenge = "Transport send error: Transport [rmcp::transport::\
            StreamableHttpClientTransport] error: unexpected server response: \
            HTTP 401 Unauthorized: ";
        let get_without_challenge = "Transport send error: Transport [rmcp::transport::\
            StreamableHttpClientTransport] error: Client error: HTTP status client error \
            (401 Unauthorized) for url (https://mcp.example.com/)";

        for error in [with_challenge, post_without_challenge, get_without_challenge] {
            assert!(is_auth_rejection(error), "{error}");
            assert!(should_refresh_connection(error), "{error}");
        }
    }

    #[test]
    fn server_errors_that_mention_authorization_are_not_auth_rejections() {
        let error = "Mcp error: -32001: upstream returned 401 Unauthorized; auth required";
        assert!(!is_auth_rejection(error));
        assert!(!should_refresh_connection(error));
    }

    #[test]
    fn a_rejected_tool_call_is_retried_without_replay_opt_in() {
        let config = RefreshConfig::default();
        let rejected = "Transport send error: Transport [http] error: Auth required";
        assert!(should_retry_mcp_operation(rejected, 0, &config, true, false));
        assert!(!should_retry_mcp_operation("Transport closed EOF", 0, &config, true, false));
    }

    #[test]
    fn test_refresh_config_default() {
        let config = RefreshConfig::default();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.retry_delay_ms, 1000);
        assert!(config.log_reconnections);
    }

    #[test]
    fn test_refresh_config_builder() {
        let config = RefreshConfig::default()
            .with_max_attempts(5)
            .with_retry_delay_ms(500)
            .without_logging();

        assert_eq!(config.max_attempts, 5);
        assert_eq!(config.retry_delay_ms, 500);
        assert!(!config.log_reconnections);
    }

    #[test]
    fn test_retry_result() {
        let ok_result = RetryResult::ok(42);
        assert_eq!(ok_result.value, 42);
        assert!(!ok_result.reconnected);

        let reconnected_result = RetryResult::reconnected(42);
        assert_eq!(reconnected_result.value, 42);
        assert!(reconnected_result.reconnected);
    }
}
