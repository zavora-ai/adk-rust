//! Token validator trait and JWT implementation.

use super::{JwksCache, TokenClaims, TokenError};
use async_trait::async_trait;
use std::sync::Arc;

/// Trait for validating tokens.
#[async_trait]
pub trait TokenValidator: Send + Sync {
    /// Validate a token and extract claims.
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError>;

    /// Get the issuer this validator handles.
    fn issuer(&self) -> &str;
}

/// JWT validator with JWKS-based signature verification.
#[cfg(feature = "sso")]
pub struct JwtValidator {
    /// Expected issuer.
    issuer: String,
    /// Expected audience.
    audience: Option<String>,
    /// JWKS cache for key lookup.
    jwks_cache: Arc<JwksCache>,
    /// Allowed algorithms.
    algorithms: Vec<jsonwebtoken::Algorithm>,
}

#[cfg(feature = "sso")]
impl JwtValidator {
    /// Create a new builder.
    pub fn builder() -> JwtValidatorBuilder {
        JwtValidatorBuilder::default()
    }

    fn validation(&self, _kid: Option<&str>) -> jsonwebtoken::Validation {
        let mut validation = jsonwebtoken::Validation::new(
            self.algorithms.first().copied().unwrap_or(jsonwebtoken::Algorithm::RS256),
        );
        validation.set_issuer(&[&self.issuer]);
        if let Some(aud) = &self.audience {
            validation.set_audience(&[aud]);
        }
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation
    }
}

#[cfg(feature = "sso")]
#[async_trait]
impl TokenValidator for JwtValidator {
    async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
        // Decode header to get key ID
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid.ok_or_else(|| TokenError::MissingClaim("kid".into()))?;

        // Get decoding key from JWKS cache
        let key = self.jwks_cache.get_key(&kid).await?;

        // Validate and decode token
        let validation = self.validation(Some(&kid));
        let token_data = jsonwebtoken::decode::<TokenClaims>(token, &key, &validation)?;

        Ok(token_data.claims)
    }

    fn issuer(&self) -> &str {
        &self.issuer
    }
}

/// Builder for JwtValidator.
#[cfg(feature = "sso")]
#[derive(Default)]
pub struct JwtValidatorBuilder {
    issuer: Option<String>,
    audience: Option<String>,
    jwks_uri: Option<String>,
    algorithms: Vec<jsonwebtoken::Algorithm>,
}

#[cfg(feature = "sso")]
impl JwtValidatorBuilder {
    /// Set the expected issuer.
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Set the expected audience.
    pub fn audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    /// Set the JWKS URI.
    pub fn jwks_uri(mut self, uri: impl Into<String>) -> Self {
        self.jwks_uri = Some(uri.into());
        self
    }

    /// Add an allowed algorithm.
    pub fn algorithm(mut self, alg: jsonwebtoken::Algorithm) -> Self {
        self.algorithms.push(alg);
        self
    }

    /// Build the validator.
    pub fn build(self) -> Result<JwtValidator, TokenError> {
        let issuer =
            self.issuer.ok_or_else(|| TokenError::ValidationError("issuer is required".into()))?;
        let jwks_uri = self
            .jwks_uri
            .ok_or_else(|| TokenError::ValidationError("jwks_uri is required".into()))?;

        let algorithms = if self.algorithms.is_empty() {
            vec![jsonwebtoken::Algorithm::RS256]
        } else {
            self.algorithms
        };

        for algorithm in &algorithms {
            match algorithm {
                jsonwebtoken::Algorithm::RS256
                | jsonwebtoken::Algorithm::RS384
                | jsonwebtoken::Algorithm::RS512
                | jsonwebtoken::Algorithm::PS256
                | jsonwebtoken::Algorithm::PS384
                | jsonwebtoken::Algorithm::PS512
                | jsonwebtoken::Algorithm::ES256
                | jsonwebtoken::Algorithm::ES384 => {}
                jsonwebtoken::Algorithm::HS256
                | jsonwebtoken::Algorithm::HS384
                | jsonwebtoken::Algorithm::HS512
                | jsonwebtoken::Algorithm::EdDSA => {
                    return Err(TokenError::ValidationError(format!(
                        "algorithm '{algorithm:?}' is not supported with JWKS-based validation. Use an RSA or EC algorithm instead."
                    )));
                }
            }
        }

        Ok(JwtValidator {
            issuer,
            audience: self.audience,
            jwks_cache: Arc::new(JwksCache::new(jwks_uri)),
            algorithms,
        })
    }
}

// Stubs for when SSO is not enabled
#[cfg(not(feature = "sso"))]
pub struct JwtValidator;

#[cfg(not(feature = "sso"))]
impl JwtValidator {
    pub fn builder() -> JwtValidatorBuilder {
        JwtValidatorBuilder
    }
}

#[cfg(not(feature = "sso"))]
pub struct JwtValidatorBuilder;

#[cfg(not(feature = "sso"))]
impl JwtValidatorBuilder {
    pub fn issuer(self, _: impl Into<String>) -> Self {
        self
    }
    pub fn audience(self, _: impl Into<String>) -> Self {
        self
    }
    pub fn jwks_uri(self, _: impl Into<String>) -> Self {
        self
    }
    pub fn build(self) -> Result<JwtValidator, TokenError> {
        Err(TokenError::ValidationError("SSO feature not enabled".into()))
    }
}
