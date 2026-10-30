//! Google provider.

use super::super::validator::REQUIRED_CLAIMS;
use super::super::{JwksCache, TokenClaims, TokenError, TokenValidator};
use async_trait::async_trait;
use std::sync::Arc;

/// Google identity provider.
pub struct GoogleProvider {
    client_id: String,
    issuer: String,
    #[cfg(feature = "sso")]
    jwks_cache: Arc<JwksCache>,
}

impl GoogleProvider {
    /// Create a new Google provider.
    ///
    /// # Arguments
    /// * `client_id` - Google OAuth client ID
    #[cfg(feature = "sso")]
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            issuer: "https://accounts.google.com".into(),
            jwks_cache: Arc::new(JwksCache::new("https://www.googleapis.com/oauth2/v3/certs")),
        }
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for GoogleProvider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".into()))?;

        let key = self.jwks_cache.get_key(&kid).await?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        // Google may use either issuer
        validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
        validation.set_audience(&[&self.client_id]);
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
impl GoogleProvider {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self { client_id: client_id.into(), issuer: String::new() }
    }
}

#[cfg(all(test, feature = "sso"))]
mod tests {
    use super::GoogleProvider;
    use crate::sso::JwksCache;
    use crate::sso::test_issuer::{TestIssuer, assert_requires_iss_and_aud};
    use std::sync::Arc;

    #[tokio::test]
    async fn tokens_need_the_iss_and_aud_claims() {
        let issuer = TestIssuer::start().await;
        let mut provider = GoogleProvider::new("client-id");
        provider.jwks_cache = Arc::new(JwksCache::new(issuer.jwks_uri()));

        assert_requires_iss_and_aud(&provider, "https://accounts.google.com", "client-id").await;
    }
}
