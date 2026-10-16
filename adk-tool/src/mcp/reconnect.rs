// MCP Connection Refresher
//
// Provides automatic reconnection for MCP connections when they fail.
// Based on adk-go's connectionRefresher pattern.
//
// Handles:
// - A closed transport, as when a stdio server process exits
// - A broken pipe, reset connection, or interrupted HTTP stream
// - An expired Streamable HTTP session
// - HTTP 401 rejections, so a factory can reconnect with fresh credentials
// - Automatic retry with reconnection

use super::discovery::{ListError, McpDiscoveryLimits, list_tools};
use rmcp::{
    RoleClient, ServiceError,
    model::{CallToolRequestParams, CallToolResponse, Tool as McpTool},
    service::RunningService,
};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// How a failed request relates to the connection that carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The connection can no longer carry requests.
    ConnectionLost,
    /// The server rejected the request's credentials with HTTP 401 before running it.
    AuthRejected,
    /// The server answered, or the request failed for a reason a new connection does not fix.
    Other,
}

/// Classifies `error` from its variant and the transport error it wraps.
///
/// Text the server chose, such as an HTTP error body, never decides the outcome.
fn classify(error: &ServiceError) -> Failure {
    match error {
        ServiceError::TransportClosed => Failure::ConnectionLost,
        ServiceError::TransportSend(error) => classify_transport(error.error.as_ref()),
        // The server answered with a JSON-RPC error or an unexpected result, or the
        // request ended on this side: none of these leaves the connection unusable.
        _ => Failure::Other,
    }
}

/// The error ADK's own Streamable HTTP client reports.
#[cfg(feature = "http-transport")]
type HttpError = rmcp::transport::streamable_http_client::StreamableHttpError<reqwest_mcp::Error>;

fn classify_transport(error: &(dyn std::error::Error + 'static)) -> Failure {
    #[cfg(feature = "http-transport")]
    if let Some(error) = error.downcast_ref::<HttpError>() {
        return classify_http(error);
    }
    let mut current = Some(error);
    while let Some(error) = current {
        #[cfg(feature = "http-transport")]
        if error.is::<rmcp::transport::streamable_http_client::AuthRequiredError>() {
            return Failure::AuthRejected;
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            return classify_io(error);
        }
        current = error.source();
    }
    Failure::Other
}

#[cfg(feature = "http-transport")]
fn classify_http(error: &HttpError) -> Failure {
    use rmcp::transport::streamable_http_client::{SseError, StreamableHttpError};
    match error {
        StreamableHttpError::AuthRequired(_) => Failure::AuthRejected,
        // rmcp formats this as `HTTP {status}: {body}`, so the prefix is rmcp's, not the server's.
        StreamableHttpError::UnexpectedServerResponse(message)
            if message.starts_with("HTTP 401 ") =>
        {
            Failure::AuthRejected
        }
        StreamableHttpError::Client(error)
            if error.status() == Some(reqwest_mcp::StatusCode::UNAUTHORIZED) =>
        {
            Failure::AuthRejected
        }
        StreamableHttpError::Io(error) => classify_io(error),
        StreamableHttpError::Sse(SseError::Body(_))
        | StreamableHttpError::UnexpectedEndOfStream
        | StreamableHttpError::TransportChannelClosed
        | StreamableHttpError::SessionExpired => Failure::ConnectionLost,
        // A 5xx, an unparseable body, or any other answer the server sent on a live session.
        _ => Failure::Other,
    }
}

fn classify_io(error: &std::io::Error) -> Failure {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::BrokenPipe
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected
        | ErrorKind::UnexpectedEof => Failure::ConnectionLost,
        _ => Failure::Other,
    }
}

/// Returns `true` when `error` shows the connection is unusable, or that the server
/// rejected the request's credentials, so a new connection may succeed.
///
/// The decision comes from the error's variant: a closed transport, a broken pipe or
/// reset connection, an interrupted HTTP stream, an expired session, or an HTTP 401.
/// An answer from the server, such as an HTTP 5xx or a JSON-RPC error, never
/// triggers a refresh, whatever its body says.
///
/// # Example
///
/// ```
/// use adk_tool::mcp::{rmcp::ServiceError, should_refresh_connection};
///
/// assert!(should_refresh_connection(&ServiceError::TransportClosed));
/// assert!(!should_refresh_connection(&ServiceError::UnexpectedResponse));
/// ```
pub fn should_refresh_connection(error: &ServiceError) -> bool {
    match classify(error) {
        Failure::ConnectionLost | Failure::AuthRejected => true,
        Failure::Other => false,
    }
}

/// Returns `true` when the transport reports that the server rejected the
/// request's credentials with HTTP 401.
///
/// rmcp reports a 401 that carries `WWW-Authenticate` as `AuthRequired`, and
/// one without it as an `HTTP 401` server response or reqwest's status error. A
/// JSON-RPC error whose message merely mentions authorization is not matched:
/// that request reached the server.
pub(crate) fn is_auth_rejection(error: &ServiceError) -> bool {
    classify(error) == Failure::AuthRejected
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
    error: &ServiceError,
    attempt: u32,
    refresh_config: &RefreshConfig,
    has_connection_factory: bool,
    replay_allowed: bool,
) -> bool {
    let failure = classify(error);
    (replay_allowed || failure == Failure::AuthRejected)
        && has_connection_factory
        && attempt < refresh_config.max_attempts
        && failure != Failure::Other
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
    /// Bounds on `list_tools`.
    discovery_limits: McpDiscoveryLimits,
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
            discovery_limits: McpDiscoveryLimits::default(),
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
            discovery_limits: McpDiscoveryLimits::default(),
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

    /// Set the bounds on [`list_tools`](Self::list_tools).
    ///
    /// The timeout covers the whole listing, reconnect attempts included. See
    /// [`McpDiscoveryLimits`] for the defaults.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let refresher = ConnectionRefresher::new(client, Arc::new(factory))
    ///     .with_discovery_limits(McpDiscoveryLimits::default().with_max_pages(8));
    /// ```
    pub fn with_discovery_limits(mut self, limits: McpDiscoveryLimits) -> Self {
        self.discovery_limits = limits;
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
        Fut: std::future::Future<Output = Result<T, ListError>>,
    {
        let mut connection = self.connection().await?;
        let mut attempt = 0u32;
        loop {
            let error = match send(Arc::clone(&connection)).await {
                Ok(value) => return Ok(RetryResult { value, reconnected: attempt > 0 }),
                Err(ListError::Service(error)) => error,
                // A listing that ran past its bounds would do the same on a new connection.
                Err(bound) => return Err(bound.to_string()),
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
                return Err(error.to_string());
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
    /// The listing is bounded by the [discovery limits](Self::with_discovery_limits).
    pub async fn list_tools(&self) -> Result<RetryResult<Vec<McpTool>>, String> {
        let limits = self.discovery_limits;
        let listing = self.with_reconnect("list_tools", true, |client| async move {
            list_tools(client.peer(), limits.max_pages).await
        });
        tokio::time::timeout(limits.timeout, listing).await.unwrap_or_else(|_| {
            Err(ListError::Timeout { method: "tools/list", timeout: limits.timeout }.to_string())
        })
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
            async move { Ok(client.call_tool_once(params).await?) }
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
    discovery_limits: McpDiscoveryLimits,
}

impl<S> SimpleClient<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new simple client wrapper.
    pub fn new(client: RunningService<RoleClient, S>) -> Self {
        Self { client: Arc::new(client), discovery_limits: McpDiscoveryLimits::default() }
    }

    /// Set the bounds on [`list_tools`](Self::list_tools). See [`McpDiscoveryLimits`]
    /// for the defaults.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let simple = SimpleClient::new(client)
    ///     .with_discovery_limits(McpDiscoveryLimits::default().with_max_pages(8));
    /// ```
    pub fn with_discovery_limits(mut self, limits: McpDiscoveryLimits) -> Self {
        self.discovery_limits = limits;
        self
    }

    /// List all tools from the MCP server, within the [discovery limits](Self::with_discovery_limits).
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, String> {
        let limits = self.discovery_limits;
        super::discovery::within(
            "tools/list",
            limits.timeout,
            list_tools(self.client.peer(), limits.max_pages),
        )
        .await
        .map_err(|error| error.to_string())
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

    fn transport_error(error: impl std::error::Error + Send + Sync + 'static) -> ServiceError {
        ServiceError::TransportSend(rmcp::transport::DynamicTransportError::from_parts(
            "test",
            std::any::TypeId::of::<()>(),
            Box::new(error),
        ))
    }

    fn io(kind: std::io::ErrorKind, message: &str) -> std::io::Error {
        std::io::Error::new(kind, message.to_string())
    }

    #[test]
    fn a_lost_connection_triggers_a_refresh() {
        use std::io::ErrorKind;
        // rmcp's `ServiceError::TransportClosed`, reported once a stdio server exits.
        assert!(should_refresh_connection(&ServiceError::TransportClosed));
        for kind in [
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
        ] {
            assert!(should_refresh_connection(&transport_error(io(kind, "lost"))), "{kind:?}");
        }
    }

    /// An I/O error a transport wraps in its own error type is found through `source()`.
    #[test]
    fn a_wrapped_io_error_is_classified_by_its_kind() {
        #[derive(Debug)]
        struct Codec(std::io::Error);
        impl std::fmt::Display for Codec {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "codec failed")
            }
        }
        impl std::error::Error for Codec {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let broken = Codec(io(std::io::ErrorKind::BrokenPipe, "pipe"));
        assert!(should_refresh_connection(&transport_error(broken)));
    }

    /// Server-chosen text that mentions a lost connection never triggers a refresh.
    #[test]
    fn error_text_does_not_trigger_a_refresh() {
        let text = "upstream geoffrey: EOF, broken pipe, connection reset, transport closed, \
                    session not found";
        let errors = [
            transport_error(io(std::io::ErrorKind::Other, text)),
            ServiceError::McpError(rmcp::ErrorData::internal_error(text.to_string(), None)),
            ServiceError::UnexpectedResponse,
            ServiceError::Timeout { timeout: std::time::Duration::from_secs(1) },
        ];
        for error in errors {
            assert!(!should_refresh_connection(&error), "{error}");
            assert!(!is_auth_rejection(&error), "{error}");
        }
    }

    #[test]
    fn server_errors_that_mention_authorization_are_not_auth_rejections() {
        let error = ServiceError::McpError(rmcp::ErrorData::new(
            rmcp::model::ErrorCode(-32001),
            "upstream returned 401 Unauthorized; auth required",
            None,
        ));
        assert!(!is_auth_rejection(&error));
        assert!(!should_refresh_connection(&error));
    }

    #[cfg(feature = "http-transport")]
    mod http {
        use super::*;
        use rmcp::transport::streamable_http_client::AuthRequiredError;

        fn http(error: HttpError) -> ServiceError {
            transport_error(error)
        }

        #[test]
        fn http_401_rejections_trigger_a_refresh() {
            let with_challenge =
                http(HttpError::AuthRequired(AuthRequiredError::new("Bearer".to_string())));
            let without_challenge =
                http(HttpError::UnexpectedServerResponse("HTTP 401 Unauthorized: ".into()));
            for error in [with_challenge, without_challenge] {
                assert!(is_auth_rejection(&error), "{error}");
                assert!(should_refresh_connection(&error), "{error}");
            }
        }

        #[test]
        fn a_5xx_never_triggers_a_refresh_whatever_its_body_says() {
            let error = http(HttpError::UnexpectedServerResponse(
                "HTTP 500 Internal Server Error: upstream geoffrey: EOF; Transport closed; \
                 connection reset; session not found; HTTP 401 Unauthorized"
                    .into(),
            ));
            assert!(!should_refresh_connection(&error), "{error}");
            assert!(!is_auth_rejection(&error), "{error}");
        }

        #[test]
        fn an_interrupted_stream_or_expired_session_triggers_a_refresh() {
            for error in [
                HttpError::UnexpectedEndOfStream,
                HttpError::TransportChannelClosed,
                HttpError::SessionExpired,
                HttpError::Io(io(std::io::ErrorKind::ConnectionReset, "reset")),
            ] {
                let error = http(error);
                assert!(should_refresh_connection(&error), "{error}");
                assert!(!is_auth_rejection(&error), "{error}");
            }
        }

        #[test]
        fn a_rejected_tool_call_is_retried_without_replay_opt_in() {
            let config = RefreshConfig::default();
            let rejected = http(HttpError::AuthRequired(AuthRequiredError::new("Bearer".into())));
            assert!(should_retry_mcp_operation(&rejected, 0, &config, true, false));
            assert!(!should_retry_mcp_operation(
                &ServiceError::TransportClosed,
                0,
                &config,
                true,
                false
            ));
        }
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
