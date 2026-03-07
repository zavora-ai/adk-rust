//! Azure AD provider.

use super::super::{JwksCache, TokenClaims, TokenError, TokenValidator};
use async_trait::async_trait;
use std::sync::Arc;

/// Azure Active Directory (Entra ID) provider.
///
/// Supports Azure AD v2.0 endpoints.
pub struct AzureADProvider {
    tenant_id: String,
    client_id: String,
    issuer: String,
    #[cfg(feature = "sso")]
    jwks_cache: Arc<JwksCache>,
}

impl AzureADProvider {
    /// Create a new Azure AD provider.
    ///
    /// # Arguments
    /// * `domain` - The domain for the OIDC provider (e.g., your Okta domain)
    /// * `client_id` - Application (client) ID
    #[cfg(feature = "sso")]
    pub fn new(tenant_id: impl Into<String>, client_id: impl Into<String>) -> Self {
        let tenant_id = tenant_id.into();
        let issuer = format!("https://login.microsoftonline.com/{}/v2.0", tenant_id);
        let jwks_uri =
            format!("https://login.microsoftonline.com/{}/discovery/v2.0/keys", tenant_id);

        Self {
            tenant_id,
            client_id: client_id.into(),
            issuer,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Create for multi-tenant applications.
    #[cfg(feature = "sso")]
    pub fn multi_tenant(client_id: impl Into<String>) -> Self {
        let issuer = "https://login.microsoftonline.com/common/v2.0".to_string();
        let jwks_uri = "https://login.microsoftonline.com/common/discovery/v2.0/keys";

        Self {
            tenant_id: "common".to_string(),
            client_id: client_id.into(),
            issuer,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Get the tenant ID.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for AzureADProvider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        // Decode header to get key ID
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".to_string()))?;

        // Get decoding key from JWKS cache
        let key = self.jwks_cache.get_key(&kid).await?;

        // Build validation
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);

        // For multi-tenant, skip issuer validation (validate manually if needed)
        if self.tenant_id != "common" {
            validation.set_issuer(&[&self.issuer]);
        }
        // If common/multi-tenant, issuer is not set so not validated
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

// Stub for when SSO is not enabled
#[cfg(not(feature = "sso"))]
impl AzureADProvider {
    pub fn new(tenant_id: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self { tenant_id: tenant_id.into(), client_id: client_id.into(), issuer: String::new() }
    }
}
