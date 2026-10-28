//! Azure AD provider.

use super::super::validator::REQUIRED_CLAIMS;
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
    allowed_tenants: Option<Vec<String>>,
    #[cfg(feature = "sso")]
    jwks_cache: Arc<JwksCache>,
}

impl AzureADProvider {
    /// Create a new Azure AD provider.
    ///
    /// # Arguments
    /// * `tenant_id` - Azure AD tenant ID (directory ID)
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
            allowed_tenants: None,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Create for multi-tenant applications.
    ///
    /// This accepts tokens from any Azure tenant that targets the configured audience.
    /// Use [`Self::with_allowed_tenants`] to restrict which tenant IDs are accepted.
    #[cfg(feature = "sso")]
    pub fn multi_tenant(client_id: impl Into<String>) -> Self {
        let issuer = "https://login.microsoftonline.com/common/v2.0".into();
        let jwks_uri = "https://login.microsoftonline.com/common/discovery/v2.0/keys";

        Self {
            tenant_id: "common".into(),
            client_id: client_id.into(),
            issuer,
            allowed_tenants: None,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
        }
    }

    /// Restrict a multi-tenant application to an explicit set of tenant IDs.
    pub fn with_allowed_tenants(
        mut self,
        tenant_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_tenants = Some(tenant_ids.into_iter().map(Into::into).collect());
        self
    }

    /// Get the tenant ID.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn validate_multi_tenant_claims(&self, claims: &TokenClaims) -> Result<(), TokenError> {
        let Some(allowed_tenants) = &self.allowed_tenants else {
            return Ok(());
        };

        let tenant_id = claims.tid.as_deref().ok_or_else(|| {
            TokenError::ValidationError(
                "azure multi-tenant tokens must include 'tid' when allowed tenants are configured. Try restricting the app to a single tenant or ensure the token includes a tenant ID."
                    .into(),
            )
        })?;

        if allowed_tenants.iter().any(|allowed| allowed == tenant_id) {
            Ok(())
        } else {
            Err(TokenError::ValidationError(format!(
                "tenant '{tenant_id}' is not allowed for this multi-tenant application. Configure AzureADProvider::with_allowed_tenants(...) with approved tenant IDs."
            )))
        }
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for AzureADProvider {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        // Decode header to get key ID
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".into()))?;

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
        // `common` checks no issuer value, but every Entra ID token carries `iss`, so the
        // claim is required on that path too.
        validation.set_required_spec_claims(&REQUIRED_CLAIMS);
        validation.validate_exp = true;
        validation.validate_nbf = true;

        // Validate and decode token
        let token_data = jsonwebtoken::decode::<TokenClaims>(token, &key, &validation)?;
        self.validate_multi_tenant_claims(&token_data.claims)?;

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
        Self {
            tenant_id: tenant_id.into(),
            client_id: client_id.into(),
            issuer: String::new(),
            allowed_tenants: None,
        }
    }
}

#[cfg(all(test, feature = "sso"))]
mod tests {
    use super::AzureADProvider;
    use crate::sso::test_issuer::{TestIssuer, assert_requires_iss_and_aud, token};
    use crate::sso::{JwksCache, TokenClaims, TokenValidator};
    use std::sync::Arc;

    #[tokio::test]
    async fn single_tenant_tokens_need_the_iss_and_aud_claims() {
        let issuer = TestIssuer::start().await;
        let mut provider = AzureADProvider::new("tenant-1", "client-id");
        provider.jwks_cache = Arc::new(JwksCache::new(issuer.jwks_uri()));

        let tenant_issuer = "https://login.microsoftonline.com/tenant-1/v2.0";
        assert_requires_iss_and_aud(&provider, tenant_issuer, "client-id").await;
    }

    /// `common` still checks no issuer value, so any tenant's token is accepted, but the token
    /// must carry an `iss` claim.
    #[tokio::test]
    async fn multi_tenant_tokens_need_an_iss_claim_of_any_value() {
        let issuer = TestIssuer::start().await;
        let mut provider = AzureADProvider::multi_tenant("client-id");
        provider.jwks_cache = Arc::new(JwksCache::new(issuer.jwks_uri()));

        let other_tenant = "https://login.microsoftonline.com/tenant-9/v2.0";
        assert_requires_iss_and_aud(&provider, other_tenant, "client-id").await;
        let any_issuer = token(Some("https://elsewhere.example"), Some("client-id"));
        assert!(provider.validate(&any_issuer).await.is_ok(), "common checks no issuer value");
    }

    #[test]
    fn test_multi_tenant_allowlist_rejects_unapproved_tenants() {
        let provider = AzureADProvider::multi_tenant("client-id")
            .with_allowed_tenants(["tenant-a", "tenant-b"]);

        assert!(
            provider
                .validate_multi_tenant_claims(&TokenClaims {
                    tid: Some("tenant-a".into()),
                    ..Default::default()
                })
                .is_ok()
        );
        assert!(
            provider
                .validate_multi_tenant_claims(&TokenClaims {
                    tid: Some("tenant-c".into()),
                    ..Default::default()
                })
                .is_err()
        );
    }
}
