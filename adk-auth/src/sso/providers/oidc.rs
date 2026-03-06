//! Generic OIDC provider.

use super::super::{JwksCache, TokenClaims, TokenError, TokenValidator};
use async_trait::async_trait;
use std::sync::Arc;

/// Generic OpenID Connect provider.
///
/// Supports any OIDC-compliant identity provider.
pub struct OidcProvider {
    issuer: String,
    client_id: String,
    jwks_cache: Arc<JwksCache>,
    #[cfg(feature = "sso")]
    algorithms: Vec<jsonwebtoken::Algorithm>,
}

impl OidcProvider {
    /// Create a new OIDC provider with manual configuration.
    #[cfg(feature = "sso")]
    pub fn new(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        jwks_uri: impl Into<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            client_id: client_id.into(),
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
            algorithms: vec![jsonwebtoken::Algorithm::RS256],
        }
    }

    /// Create from OIDC discovery endpoint.
    #[cfg(feature = "sso")]
    pub async fn from_discovery(
        issuer_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self, TokenError> {
        let issuer = issuer_url.into();
        let discovery_url =
            format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let response = client
            .get(&discovery_url)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| TokenError::DiscoveryError(e.to_string()))?;

        let config: OidcConfig =
            response.json().await.map_err(|e| TokenError::DiscoveryError(e.to_string()))?;

        Ok(Self {
            issuer: config.issuer,
            client_id: client_id.into(),
            jwks_cache: Arc::new(JwksCache::new(config.jwks_uri)),
            algorithms: vec![jsonwebtoken::Algorithm::RS256],
        })
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for OidcProvider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        // Decode header to get key ID
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".to_string()))?;

        // Get decoding key from JWKS cache
        let key = self.jwks_cache.get_key(&kid).await?;

        // Build validation
        let mut validation = jsonwebtoken::Validation::new(
            self.algorithms.first().copied().unwrap_or(jsonwebtoken::Algorithm::RS256),
        );
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.validate_exp = true;

        // Validate and decode token
        let token_data = jsonwebtoken::decode::<TokenClaims>(token, &key, &validation)?;

        Ok(token_data.claims)
    }

    fn issuer(&self) -> &str {
        &self.issuer
    }
}

/// OIDC discovery configuration.
#[cfg(feature = "sso")]
#[derive(Debug, serde::Deserialize)]
struct OidcConfig {
    issuer: String,
    jwks_uri: String,
    #[allow(dead_code)]
    authorization_endpoint: Option<String>,
    #[allow(dead_code)]
    token_endpoint: Option<String>,
}

// Stub for when SSO is not enabled
#[cfg(not(feature = "sso"))]
impl OidcProvider {
    pub fn new(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        _jwks_uri: impl Into<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            client_id: client_id.into(),
            jwks_cache: Arc::new(JwksCache::new("")),
        }
    }
}
