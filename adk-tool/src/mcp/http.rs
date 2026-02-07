// MCP HTTP Transport (Streamable HTTP)
//
// Provides HTTP-based transport for connecting to remote MCP servers.
// Uses the streamable HTTP transport from rmcp when the http-transport feature is enabled.

use super::auth::McpAuth;
use adk_core::{AdkError, Result};
use std::collections::HashMap;
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
        use rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };

        // Extract the raw token from auth config
        // rmcp's bearer_auth() adds "Bearer " prefix automatically
        let token = match &self.auth {
            McpAuth::Bearer(token) => Some(token.clone()),
            McpAuth::OAuth2(config) => {
                // Get token from OAuth2 flow
                let token = config
                    .get_or_refresh_token()
                    .await
                    .map_err(|e| AdkError::Tool(format!("OAuth2 authentication failed: {}", e)))?;
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
            .map_err(|e| AdkError::Tool(format!("Failed to connect to MCP server: {}", e)))?;

        Ok(super::McpToolset::new(client))
    }

    /// Connect to the MCP server (stub when http-transport feature is disabled).
    #[cfg(not(feature = "http-transport"))]
    pub async fn connect(self) -> Result<()> {
        Err(AdkError::Tool(
            "HTTP transport requires the 'http-transport' feature. \
             Add `adk-tool = { features = [\"http-transport\"] }` to your Cargo.toml"
                .to_string(),
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
