// MCP HTTP Transport (Streamable HTTP)
//
// Provides HTTP-based transport for connecting to remote MCP servers.
// Uses the streamable HTTP transport from rmcp when the http-transport feature is enabled.

use super::auth::McpAuth;
use super::elicitation::ElicitationHandler;
use adk_core::{AdkError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Builder for HTTP-based MCP connections.
///
/// This builder creates connections to remote MCP servers using the
/// streamable HTTP transport (SEP-1686 compliant).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::mcp::{McpHttpClientBuilder, McpAuth, OAuth2Config};
///
/// // Simple connection
/// let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
///     .connect()
///     .await?;
///
/// // With OAuth2 authentication
/// let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
///     .with_auth(McpAuth::oauth2(
///         OAuth2Config::new("client-id", "https://auth.example.com/token")
///             .with_secret("client-secret")
///             .with_scopes(vec!["mcp:read".into()])
///     ))
///     .timeout(Duration::from_secs(60))
///     .connect()
///     .await?;
/// ```
#[derive(Clone)]
pub struct McpHttpClientBuilder {
    /// MCP server endpoint URL
    endpoint: String,
    /// Authentication configuration
    auth: McpAuth,
    /// Request timeout
    timeout: Duration,
    /// Custom headers
    headers: HashMap<String, String>,
    /// Optional elicitation handler
    elicitation_handler: Option<Arc<dyn ElicitationHandler>>,
}

impl McpHttpClientBuilder {
    /// Create a new HTTP client builder for the given endpoint.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - The MCP server URL (e.g., "https://mcp.example.com/v1")
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            auth: McpAuth::None,
            timeout: Duration::from_secs(30),
            headers: HashMap::new(),
            elicitation_handler: None,
        }
    }

    /// Set authentication for the connection.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = McpHttpClientBuilder::new("https://mcp.example.com")
    ///     .with_auth(McpAuth::bearer("my-token"));
    /// ```
    pub fn with_auth(mut self, auth: McpAuth) -> Self {
        self.auth = auth;
        self
    }

    /// Set the request timeout.
    ///
    /// Default is 30 seconds.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Add a custom header to all requests.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Configure an elicitation handler for the HTTP connection.
    ///
    /// When set, use [`connect_with_elicitation`](Self::connect_with_elicitation)
    /// to create a toolset that advertises elicitation capabilities.
    pub fn with_elicitation_handler(mut self, handler: Arc<dyn ElicitationHandler>) -> Self {
        self.elicitation_handler = Some(handler);
        self
    }

    /// Get the endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Get the configured timeout.
    pub fn get_timeout(&self) -> Duration {
        self.timeout
    }

    /// Get the authentication configuration.
    pub fn get_auth(&self) -> &McpAuth {
        &self.auth
    }

    /// Connect to the MCP server and create a toolset.
    ///
    /// This method establishes a connection to the remote MCP server
    /// using the streamable HTTP transport.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The `http-transport` feature is not enabled
    /// - Connection to the server fails
    /// - Authentication fails
    #[cfg(feature = "http-transport")]
    pub async fn connect(
        self,
    ) -> Result<super::McpToolset<impl rmcp::service::Service<rmcp::RoleClient>>> {
        use adk_core::{ErrorCategory, ErrorComponent};
        use rmcp::ServiceExt;
        use rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };

        // Extract the raw token from auth config
        // rmcp's bearer_auth() adds "Bearer " prefix automatically
        let token = match &self.auth {
            McpAuth::Bearer(token) => Some(token.clone()),
            McpAuth::OAuth2(config) => {
                // Get token from OAuth2 flow
                let token = config.get_or_refresh_token().await.map_err(|e| {
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Unauthorized,
                        "mcp.oauth.token_fetch",
                        format!("OAuth2 authentication failed: {e}"),
                    )
                })?;
                Some(token)
            }
            McpAuth::ApiKey { .. } => {
                // API key auth not supported via rmcp's auth_header (uses different header)
                // Would need custom client implementation
                None
            }
            McpAuth::None => None,
        };

        // Build transport config with authentication
        let mut config = StreamableHttpClientTransportConfig::with_uri(self.endpoint.as_str());

        // Set auth header if we have a token (rmcp adds "Bearer " prefix via bearer_auth)
        if let Some(token) = token {
            config = config.auth_header(token);
        }

        // Create transport with config
        let transport = StreamableHttpClientTransport::from_config(config);

        // Connect using the service extension
        let client = ()
            .serve(transport)
            .await
            .map_err(|e| AdkError::tool(format!("Failed to connect to MCP server: {e}")))?;

        Ok(super::McpToolset::new(client))
    }

    /// Connect to the MCP server (stub when http-transport feature is disabled).
    #[cfg(not(feature = "http-transport"))]
    pub async fn connect(self) -> Result<()> {
        Err(AdkError::tool(
            "HTTP transport requires the 'http-transport' feature. \
             Add `adk-tool = { features = [\"http-transport\"] }` to your Cargo.toml",
        ))
    }

    /// Connect with elicitation support.
    ///
    /// Requires [`with_elicitation_handler`](Self::with_elicitation_handler) to have been called.
    /// Returns a `McpToolset<AdkClientHandler>` that advertises elicitation capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if no elicitation handler was configured or if the connection fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_tool::{McpHttpClientBuilder, AutoDeclineElicitationHandler};
    /// use std::sync::Arc;
    ///
    /// let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    ///     .with_elicitation_handler(Arc::new(AutoDeclineElicitationHandler))
    ///     .connect_with_elicitation()
    ///     .await?;
    /// ```
    #[cfg(feature = "http-transport")]
    pub async fn connect_with_elicitation(
        self,
    ) -> Result<super::McpToolset<impl rmcp::service::Service<rmcp::RoleClient>>> {
        use adk_core::{ErrorCategory, ErrorComponent};
        use rmcp::ServiceExt;
        use rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };

        let handler = self.elicitation_handler.ok_or_else(|| {
            AdkError::tool(
                "connect_with_elicitation requires with_elicitation_handler to be called first",
            )
        })?;

        // Extract the raw token from auth config
        let token = match &self.auth {
            McpAuth::Bearer(token) => Some(token.clone()),
            McpAuth::OAuth2(config) => {
                let token = config.get_or_refresh_token().await.map_err(|e| {
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Unauthorized,
                        "mcp.oauth.token_fetch",
                        format!("OAuth2 authentication failed: {e}"),
                    )
                })?;
                Some(token)
            }
            McpAuth::ApiKey { .. } => None,
            McpAuth::None => None,
        };

        let mut config = StreamableHttpClientTransportConfig::with_uri(self.endpoint.as_str());
        if let Some(token) = token {
            config = config.auth_header(token);
        }

        let transport = StreamableHttpClientTransport::from_config(config);
        let adk_handler = super::elicitation::AdkClientHandler::new(handler);
        let client = adk_handler
            .serve(transport)
            .await
            .map_err(|e| AdkError::tool(format!("failed to connect to MCP server: {e}")))?;

        Ok(super::McpToolset::new(client))
    }

    /// Connect with elicitation support (stub when http-transport feature is disabled).
    #[cfg(not(feature = "http-transport"))]
    pub async fn connect_with_elicitation(self) -> Result<()> {
        Err(AdkError::tool(
            "HTTP transport requires the 'http-transport' feature. \
             Add `adk-tool = { features = [\"http-transport\"] }` to your Cargo.toml",
        ))
    }
}

impl std::fmt::Debug for McpHttpClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHttpClientBuilder")
            .field("endpoint", &self.endpoint)
            .field("auth", &self.auth)
            .field("timeout", &self.timeout)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_new() {
        let builder = McpHttpClientBuilder::new("https://mcp.example.com");
        assert_eq!(builder.endpoint(), "https://mcp.example.com");
        assert_eq!(builder.get_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn test_builder_with_auth() {
        let builder = McpHttpClientBuilder::new("https://mcp.example.com")
            .with_auth(McpAuth::bearer("test-token"));
        assert!(builder.get_auth().is_configured());
    }

    #[test]
    fn test_builder_timeout() {
        let builder =
            McpHttpClientBuilder::new("https://mcp.example.com").timeout(Duration::from_secs(60));
        assert_eq!(builder.get_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn test_builder_headers() {
        let builder =
            McpHttpClientBuilder::new("https://mcp.example.com").header("X-Custom", "value");
        assert!(builder.headers.contains_key("X-Custom"));
    }
}
