// MCP Connection Refresher
//
// Provides automatic reconnection for MCP connections when they fail.
// Based on adk-go's connectionRefresher pattern.
//
// Handles:
// - Connection closed errors
// - EOF errors
// - Session not found errors
// - Automatic retry with reconnection

use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, Tool as McpTool},
    service::RunningService,
};
use std::sync::Arc;
use tokio::sync::Mutex;
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

    // Transport errors
    if error_lower.contains("transport error") || error_lower.contains("connection reset") {
        return true;
    }

    false
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
/// // Operations automatically retry on connection failure
/// let tools = refresher.list_tools().await?;
/// let result = refresher.call_tool(params).await?;
/// ```
pub struct ConnectionRefresher<S, F>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
    F: ConnectionFactory<S>,
{
    /// The current MCP client session
    client: Arc<Mutex<Option<RunningService<RoleClient, S>>>>,
    /// Factory for creating new connections
    factory: Arc<F>,
    /// Configuration for refresh behavior
    config: RefreshConfig,
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
            client: Arc::new(Mutex::new(Some(client))),
            factory,
            config: RefreshConfig::default(),
        }
    }

    /// Create a new connection refresher without an initial connection.
    ///
    /// The first operation will trigger a connection.
    pub fn lazy(factory: Arc<F>) -> Self {
        Self { client: Arc::new(Mutex::new(None)), factory, config: RefreshConfig::default() }
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

    /// Ensure we have a valid connection, creating one if needed.
    async fn ensure_connected(&self) -> Result<(), String> {
        let mut guard = self.client.lock().await;

        if guard.is_none() {
            if self.config.log_reconnections {
                info!("MCP client not connected, creating connection");
            }
            let new_client = self.factory.create_connection().await?;
            *guard = Some(new_client);
        }

        Ok(())
    }

    /// Refresh the connection by creating a new client.
    async fn refresh_connection(&self) -> Result<(), String> {
        let mut guard = self.client.lock().await;

        // Close existing connection if any
        if let Some(old_client) = guard.take() {
            if self.config.log_reconnections {
                debug!("Closing old MCP connection");
            }
            let token = old_client.cancellation_token();
            token.cancel();
            // Give it a moment to clean up
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        if self.config.log_reconnections {
            info!("Refreshing MCP connection");
        }
        let new_client = self.factory.create_connection().await?;
        *guard = Some(new_client);

        Ok(())
    }

    /// List all tools from the MCP server with automatic reconnection.
    ///
    /// Handles pagination internally and restarts from scratch if
    /// reconnection occurs (per MCP spec, cursors don't persist across sessions).
    pub async fn list_tools(&self) -> Result<RetryResult<Vec<McpTool>>, String> {
        // Ensure we have a connection
        self.ensure_connected().await?;

        // First attempt
        {
            let guard = self.client.lock().await;
            if let Some(ref client) = *guard {
                match client.list_all_tools().await {
                    Ok(tools) => return Ok(RetryResult::ok(tools)),
                    Err(e) => {
                        let error_str = e.to_string();
                        if !should_refresh_connection(&error_str) {
                            return Err(error_str);
                        }
                        if self.config.log_reconnections {
                            warn!(error = %error_str, "list_tools failed, will retry with reconnection");
                        }
                    }
                }
            }
        }

        // Retry with reconnection
        for attempt in 1..=self.config.max_attempts {
            if self.config.log_reconnections {
                info!(
                    attempt = attempt,
                    max = self.config.max_attempts,
                    "Reconnection attempt for list_tools"
                );
            }

            // Wait before retry
            if self.config.retry_delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(self.config.retry_delay_ms))
                    .await;
            }

            // Try to refresh
            if let Err(e) = self.refresh_connection().await {
                if self.config.log_reconnections {
                    warn!(error = %e, attempt = attempt, "Refresh failed");
                }
                continue;
            }

            // Retry operation
            let guard = self.client.lock().await;
            if let Some(ref client) = *guard {
                match client.list_all_tools().await {
                    Ok(tools) => {
                        if self.config.log_reconnections {
                            debug!(
                                attempt = attempt,
                                tool_count = tools.len(),
                                "list_tools succeeded after reconnection"
                            );
                        }
                        return Ok(RetryResult::reconnected(tools));
                    }
                    Err(e) => {
                        if self.config.log_reconnections {
                            warn!(error = %e, attempt = attempt, "list_tools failed after reconnection");
                        }
                    }
                }
            }
        }

        // Final attempt
        let guard = self.client.lock().await;
        if let Some(ref client) = *guard {
            client.list_all_tools().await.map(RetryResult::ok).map_err(|e| e.to_string())
        } else {
            Err("No MCP client available".to_string())
        }
    }

    /// Call a tool on the MCP server with automatic reconnection.
    pub async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<RetryResult<CallToolResult>, String> {
        // Ensure we have a connection
        self.ensure_connected().await?;

        // First attempt
        {
            let guard = self.client.lock().await;
            if let Some(ref client) = *guard {
                match client.call_tool(params.clone()).await {
                    Ok(result) => return Ok(RetryResult::ok(result)),
                    Err(e) => {
                        let error_str = e.to_string();
                        if !should_refresh_connection(&error_str) {
                            return Err(error_str);
                        }
                        if self.config.log_reconnections {
                            warn!(error = %error_str, tool = %params.name, "call_tool failed, will retry with reconnection");
                        }
                    }
                }
            }
        }

        // Retry with reconnection
        for attempt in 1..=self.config.max_attempts {
            if self.config.log_reconnections {
                info!(attempt = attempt, max = self.config.max_attempts, tool = %params.name, "Reconnection attempt for call_tool");
            }

            // Wait before retry
            if self.config.retry_delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(self.config.retry_delay_ms))
                    .await;
            }

            // Try to refresh
            if let Err(e) = self.refresh_connection().await {
                if self.config.log_reconnections {
                    warn!(error = %e, attempt = attempt, "Refresh failed");
                }
                continue;
            }

            // Retry operation
            let guard = self.client.lock().await;
            if let Some(ref client) = *guard {
                match client.call_tool(params.clone()).await {
                    Ok(result) => {
                        if self.config.log_reconnections {
                            debug!(attempt = attempt, tool = %params.name, "call_tool succeeded after reconnection");
                        }
                        return Ok(RetryResult::reconnected(result));
                    }
                    Err(e) => {
                        if self.config.log_reconnections {
                            warn!(error = %e, attempt = attempt, "call_tool failed after reconnection");
                        }
                    }
                }
            }
        }

        // Final attempt
        let guard = self.client.lock().await;
        if let Some(ref client) = *guard {
            client.call_tool(params).await.map(RetryResult::ok).map_err(|e| e.to_string())
        } else {
            Err("No MCP client available".to_string())
        }
    }

    /// Get the cancellation token for the current connection.
    pub async fn cancellation_token(
        &self,
    ) -> Option<rmcp::service::RunningServiceCancellationToken> {
        let guard = self.client.lock().await;
        guard.as_ref().map(|c| c.cancellation_token())
    }

    /// Check if currently connected.
    pub async fn is_connected(&self) -> bool {
        let guard = self.client.lock().await;
        guard.is_some()
    }

    /// Force a reconnection.
    pub async fn reconnect(&self) -> Result<(), String> {
        self.refresh_connection().await
    }

    /// Close the connection.
    pub async fn close(&self) {
        let mut guard = self.client.lock().await;
        if let Some(client) = guard.take() {
            let token = client.cancellation_token();
            token.cancel();
        }
    }
}

/// Simple wrapper for MCP clients that don't support reconnection.
///
/// Use this for stdio-based MCP servers where reconnection isn't possible
/// without restarting the server process.
pub struct SimpleClient<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    client: Arc<Mutex<RunningService<RoleClient, S>>>,
}

impl<S> SimpleClient<S>
where
    S: rmcp::service::Service<RoleClient> + Send + Sync + 'static,
{
    /// Create a new simple client wrapper.
    pub fn new(client: RunningService<RoleClient, S>) -> Self {
        Self { client: Arc::new(Mutex::new(client)) }
    }

    /// List all tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, String> {
        let client = self.client.lock().await;
        client.list_all_tools().await.map_err(|e| e.to_string())
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(&self, params: CallToolRequestParams) -> Result<CallToolResult, String> {
        let client = self.client.lock().await;
        client.call_tool(params).await.map_err(|e| e.to_string())
    }

    /// Get the cancellation token.
    pub async fn cancellation_token(&self) -> rmcp::service::RunningServiceCancellationToken {
        let client = self.client.lock().await;
        client.cancellation_token()
    }

    /// Get access to the underlying client mutex.
    pub fn inner(&self) -> &Arc<Mutex<RunningService<RoleClient, S>>> {
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

        // Should not refresh for other errors
        assert!(!should_refresh_connection("invalid argument"));
        assert!(!should_refresh_connection("permission denied"));
        assert!(!should_refresh_connection("tool not found"));
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
