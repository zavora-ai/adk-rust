//! Auth0 provider.

use super::super::validator::REQUIRED_CLAIMS;
use super::super::{JwksCache, TokenClaims, TokenError, TokenValidator};
use async_trait::async_trait;
use std::sync::Arc;

/// Auth0 identity provider.
pub struct Auth0Provider {
    domain: String,
    audience: String,
    issuer: String,
    #[cfg(feature = "sso")]
    jwks_cache: Arc<JwksCache>,
}

impl Auth0Provider {
    /// Create a new Auth0 provider.
    ///
    /// # Arguments
    /// * `domain` - Auth0 domain (e.g., "your-tenant.auth0.com")
    /// * `audience` - API audience/identifier
    #[cfg(feature = "sso")]
    pub fn new(domain: impl Into<String>, audience: impl Into<String>) -> Self {
        let domain = domain.into();
        let issuer = format!("https://{}/", domain);
        let jwks_uri = format!("https://{}/.well-known/jwks.json", domain);

        Self {
            domain,
            audience: audience.into(),
            issuer,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Get the domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Get the audience.
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for Auth0Provider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".into()))?;

        let key = self.jwks_cache.get_key(&kid).await?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&REQUIRED_CLAIMS);
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
impl Auth0Provider {
    pub fn new(domain: impl Into<String>, audience: impl Into<String>) -> Self {
        Self { domain: domain.into(), audience: audience.into(), issuer: String::new() }
    }
}

#[cfg(all(test, feature = "sso"))]
mod tests {
    use super::Auth0Provider;
    use crate::sso::JwksCache;
    use crate::sso::test_issuer::{TestIssuer, assert_requires_iss_and_aud};
    use std::sync::Arc;

    #[tokio::test]
    async fn tokens_need_the_iss_and_aud_claims() {
        let issuer = TestIssuer::start().await;
        let mut provider = Auth0Provider::new("tenant.auth0.com", "https://api.example.com");
        provider.jwks_cache = Arc::new(JwksCache::new(issuer.jwks_uri()));

        assert_requires_iss_and_aud(
            &provider,
            "https://tenant.auth0.com/",
            "https://api.example.com",
        )
        .await;
    }
}
