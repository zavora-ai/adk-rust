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
/// current MCP Streamable HTTP transport.
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
    /// Recreate the MCP session once when a remote HTTP session expires.
    reinit_on_expired_session: bool,
}

impl McpHttpClientBuilder {
    /// Create a new HTTP client builder for the given endpoint.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - The MCP server URL (e.g., `https://mcp.example.com/v1`)
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            auth: McpAuth::None,
            timeout: Duration::from_secs(30),
            headers: HashMap::new(),
            elicitation_handler: None,
            reinit_on_expired_session: true,
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

    /// Configure bounded automatic recovery when the server reports an expired session.
    ///
    /// Enabled by default. rmcp performs at most one re-initialization attempt.
    pub fn reinit_on_expired_session(mut self, enabled: bool) -> Self {
        self.reinit_on_expired_session = enabled;
        self
    }

    #[cfg(feature = "http-transport")]
    async fn build_transport(
        &self,
    ) -> Result<
        rmcp::transport::streamable_http_client::StreamableHttpClientTransport<reqwest_mcp::Client>,
    > {
        use adk_core::{ErrorCategory, ErrorComponent};
        use reqwest_mcp::header::{HeaderName, HeaderValue};
        use rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };

        let mut custom_headers = HashMap::new();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                AdkError::tool(format!("invalid MCP HTTP header '{name}': {error}"))
            })?;
            let value = HeaderValue::from_str(value).map_err(|error| {
                AdkError::tool(format!("invalid value for MCP HTTP header '{name}': {error}"))
            })?;
            custom_headers.insert(name, value);
        }

        let token = match &self.auth {
            McpAuth::Bearer(token) => Some(token.clone()),
            McpAuth::OAuth2(config) => {
                Some(config.get_or_refresh_token().await.map_err(|error| {
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Unauthorized,
                        "mcp.oauth.token_fetch",
                        format!("OAuth2 client-credentials authentication failed: {error}"),
                    )
                })?)
            }
            McpAuth::ApiKey { header, key } => {
                let name = HeaderName::from_bytes(header.as_bytes()).map_err(|error| {
                    AdkError::tool(format!("invalid MCP API-key header '{header}': {error}"))
                })?;
                let value = HeaderValue::from_str(key).map_err(|error| {
                    AdkError::tool(format!("invalid MCP API-key value for '{header}': {error}"))
                })?;
                custom_headers.insert(name, value);
                None
            }
            McpAuth::None => None,
        };

        let mut config = StreamableHttpClientTransportConfig::with_uri(self.endpoint.as_str())
            .custom_headers(custom_headers)
            .reinit_on_expired_session(self.reinit_on_expired_session);
        if let Some(token) = token {
            config = config.auth_header(token);
        }

        let client = reqwest_mcp::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|error| AdkError::tool(format!("failed to build MCP HTTP client: {error}")))?;
        Ok(StreamableHttpClientTransport::with_client(client, config))
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
        use rmcp::ServiceExt;
        let transport = self.build_transport().await?;

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
        use rmcp::ServiceExt;

        let handler = self.elicitation_handler.clone().ok_or_else(|| {
            AdkError::tool(
                "connect_with_elicitation requires with_elicitation_handler to be called first",
            )
        })?;

        let transport = self.build_transport().await?;
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
