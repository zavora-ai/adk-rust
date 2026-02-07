// MCP Authentication Support
//
// Provides authentication mechanisms for remote MCP servers.
// Integrates with adk-auth for SSO/OAuth support.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Authentication configuration for MCP connections
#[derive(Clone, Default)]
pub enum McpAuth {
    /// No authentication required
    #[default]
    None,
    /// Static bearer token
    Bearer(String),
    /// API key in header
    ApiKey { header: String, key: String },
    /// OAuth2 with automatic token refresh
    OAuth2(Arc<OAuth2Config>),
}

impl std::fmt::Debug for McpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpAuth::None => write!(f, "McpAuth::None"),
            McpAuth::Bearer(_) => write!(f, "McpAuth::Bearer([REDACTED])"),
            McpAuth::ApiKey { header, .. } => write!(f, "McpAuth::ApiKey {{ header: {} }}", header),
            McpAuth::OAuth2(_) => write!(f, "McpAuth::OAuth2([CONFIG])"),
        }
    }
}

impl McpAuth {
    /// Create bearer token auth
    pub fn bearer(token: impl Into<String>) -> Self {
        McpAuth::Bearer(token.into())
    }

    /// Create API key auth
    pub fn api_key(header: impl Into<String>, key: impl Into<String>) -> Self {
        McpAuth::ApiKey { header: header.into(), key: key.into() }
    }

    /// Create OAuth2 auth
    pub fn oauth2(config: OAuth2Config) -> Self {
        McpAuth::OAuth2(Arc::new(config))
    }

    /// Get authorization headers for a request
    pub async fn get_headers(&self) -> Result<HashMap<String, String>, AuthError> {
        let mut headers = HashMap::new();

        match self {
            McpAuth::None => {}
            McpAuth::Bearer(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {}", token));
            }
            McpAuth::ApiKey { header, key } => {
                headers.insert(header.clone(), key.clone());
            }
            McpAuth::OAuth2(config) => {
                let token = config.get_or_refresh_token().await?;
                headers.insert("Authorization".to_string(), format!("Bearer {}", token));
            }
        }

        Ok(headers)
    }

    /// Check if authentication is configured
    pub fn is_configured(&self) -> bool {
        !matches!(self, McpAuth::None)
    }
}

/// OAuth2 configuration for MCP authentication
pub struct OAuth2Config {
    /// OAuth2 client ID
    pub client_id: String,
    /// OAuth2 client secret (optional for public clients)
    pub client_secret: Option<String>,
    /// Token endpoint URL
    pub token_url: String,
    /// Requested scopes
    pub scopes: Vec<String>,
    /// Cached token with expiry
    token_cache: RwLock<Option<CachedToken>>,
}

impl OAuth2Config {
    /// Create a new OAuth2 config
    pub fn new(client_id: impl Into<String>, token_url: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: None,
            token_url: token_url.into(),
            scopes: Vec::new(),
            token_cache: RwLock::new(None),
        }
    }

    /// Set client secret
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(secret.into());
        self
    }

    /// Add scopes
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Get or refresh the access token
    pub async fn get_or_refresh_token(&self) -> Result<String, AuthError> {
        // Check cache first
        {
            let cache = self.token_cache.read().await;
            if let Some(ref cached) = *cache {
                if !cached.is_expired() {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        // Need to refresh
        let token = self.fetch_token().await?;

        // Update cache
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some(token.clone());
        }

        Ok(token.access_token)
    }

    /// Fetch a new token from the token endpoint
    async fn fetch_token(&self) -> Result<CachedToken, AuthError> {
        // Build request body
        let mut params = vec![
            ("grant_type", "client_credentials".to_string()),
            ("client_id", self.client_id.clone()),
        ];

        if let Some(ref secret) = self.client_secret {
            params.push(("client_secret", secret.clone()));
        }

        if !self.scopes.is_empty() {
            params.push(("scope", self.scopes.join(" ")));
        }

        // Make request (using reqwest if available)
        #[cfg(feature = "http-transport")]
        {
            let client = reqwest::Client::new();
            let response = client
                .post(&self.token_url)
                .form(&params)
                .send()
                .await
                .map_err(|e| AuthError::TokenFetch(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AuthError::TokenFetch(format!(
                    "Token request failed: {} - {}",
                    status, body
                )));
            }

            let token_response: TokenResponse =
                response.json().await.map_err(|e| AuthError::TokenParse(e.to_string()))?;

            Ok(CachedToken::from_response(token_response))
        }

        #[cfg(not(feature = "http-transport"))]
        {
            Err(AuthError::NotSupported("OAuth2 requires the 'http-transport' feature".to_string()))
        }
    }

    /// Clear the token cache (force refresh on next request)
    pub async fn clear_cache(&self) {
        let mut cache = self.token_cache.write().await;
        *cache = None;
    }
}

/// Cached OAuth2 token
#[derive(Clone)]
#[allow(dead_code)] // Used when http-transport feature is enabled
struct CachedToken {
    access_token: String,
    expires_at: Option<std::time::Instant>,
    refresh_token: Option<String>,
}

#[allow(dead_code)] // Used when http-transport feature is enabled
impl CachedToken {
    fn from_response(response: TokenResponse) -> Self {
        let expires_at = response.expires_in.map(|secs| {
            // Refresh 60 seconds before actual expiry
            std::time::Instant::now() + std::time::Duration::from_secs(secs.saturating_sub(60))
        });

        Self {
            access_token: response.access_token,
            expires_at,
            refresh_token: response.refresh_token,
        }
    }

    fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => std::time::Instant::now() >= expires_at,
            None => false, // No expiry = never expires
        }
    }
}

/// OAuth2 token response
#[derive(serde::Deserialize)]
#[allow(dead_code)] // Used when http-transport feature is enabled
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

/// Authentication errors
#[derive(Debug, Clone)]
pub enum AuthError {
    /// Failed to fetch token
    TokenFetch(String),
    /// Failed to parse token response
    TokenParse(String),
    /// Token expired and refresh failed
    TokenExpired(String),
    /// Feature not supported
    NotSupported(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::TokenFetch(msg) => write!(f, "Token fetch failed: {}", msg),
            AuthError::TokenParse(msg) => write!(f, "Token parse failed: {}", msg),
            AuthError::TokenExpired(msg) => write!(f, "Token expired: {}", msg),
            AuthError::NotSupported(msg) => write!(f, "Not supported: {}", msg),
        }
    }
}

impl std::error::Error for AuthError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_auth_none() {
        let auth = McpAuth::None;
        assert!(!auth.is_configured());
    }

    #[test]
    fn test_mcp_auth_bearer() {
        let auth = McpAuth::bearer("test-token");
        assert!(auth.is_configured());
    }

    #[test]
    fn test_mcp_auth_api_key() {
        let auth = McpAuth::api_key("X-API-Key", "secret-key");
        assert!(auth.is_configured());
    }

    #[tokio::test]
    async fn test_bearer_headers() {
        let auth = McpAuth::bearer("my-token");
        let headers = auth.get_headers().await.unwrap();
        assert_eq!(headers.get("Authorization"), Some(&"Bearer my-token".to_string()));
    }

    #[tokio::test]
    async fn test_api_key_headers() {
        let auth = McpAuth::api_key("X-API-Key", "secret");
        let headers = auth.get_headers().await.unwrap();
        assert_eq!(headers.get("X-API-Key"), Some(&"secret".to_string()));
    }

    #[test]
    fn test_oauth2_config() {
        let config = OAuth2Config::new("client-id", "https://auth.example.com/token")
            .with_secret("client-secret")
            .with_scopes(vec!["read".to_string(), "write".to_string()]);

        assert_eq!(config.client_id, "client-id");
        assert_eq!(config.client_secret, Some("client-secret".to_string()));
        assert_eq!(config.scopes, vec!["read", "write"]);
    }
}
