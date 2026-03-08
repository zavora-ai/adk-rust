//! Okta provider.

use super::super::{JwksCache, TokenClaims, TokenError, TokenValidator};
use async_trait::async_trait;
use std::sync::Arc;

/// Okta identity provider.
pub struct OktaProvider {
    domain: String,
    client_id: String,
    issuer: String,
    #[cfg(feature = "sso")]
    jwks_cache: Arc<JwksCache>,
}

impl OktaProvider {
    /// Create a new Okta provider.
    ///
    /// # Arguments
    /// * `domain` - Okta domain (e.g., "dev-123456.okta.com")
    /// * `client_id` - Application client ID
    #[cfg(feature = "sso")]
    pub fn new(domain: impl Into<String>, client_id: impl Into<String>) -> Self {
        let domain = domain.into();
        let issuer = format!("https://{}/oauth2/default", domain);
        let jwks_uri = format!("https://{}/oauth2/default/v1/keys", domain);

        Self {
            domain,
            client_id: client_id.into(),
            issuer,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Create with custom authorization server.
    #[cfg(feature = "sso")]
    pub fn with_auth_server(
        domain: impl Into<String>,
        auth_server_id: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Self {
        let domain = domain.into();
        let auth_server = auth_server_id.into();
        let issuer = format!("https://{}/oauth2/{}", domain, auth_server);
        let jwks_uri = format!("https://{}/oauth2/{}/v1/keys", domain, auth_server);

        Self {
            domain,
            client_id: client_id.into(),
            issuer,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Get the domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for OktaProvider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".into()))?;

        let key = self.jwks_cache.get_key(&kid).await?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.validate_exp = true;
        validation.validate_nbf = true;

        let token_data = jsonwebtoken::decode::<TokenClaims>(token, &key, &validation)?;

        Ok(token_data.claims)
    }

    fn issuer(&self) -> &str {
        &self.issuer
    }
}

// Stub for when SSO is not enabled
#[cfg(not(feature = "sso"))]
impl OktaProvider {
    pub fn new(domain: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self { domain: domain.into(), client_id: client_id.into(), issuer: String::new() }
    }
}
