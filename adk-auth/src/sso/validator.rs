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

/// The claims every validator in this module requires a token to carry.
#[cfg(feature = "sso")]
pub(crate) const REQUIRED_CLAIMS: [&str; 3] = ["exp", "iss", "aud"];

/// JWT validator with JWKS-based signature verification.
///
/// A token must carry an `exp` claim, an `iss` claim naming the configured issuer, and an
/// `aud` claim naming the configured audience. A token without one of these claims fails
/// with [`TokenError::MissingClaim`]; one naming another issuer or audience fails with
/// [`TokenError::ValidationError`]. A validator built with
/// [`JwtValidatorBuilder::without_audience`] requires no `aud` claim and refuses a token
/// that carries one.
///
/// # Example
///
/// ```rust
/// use adk_auth::sso::JwtValidator;
///
/// let validator = JwtValidator::builder()
///     .issuer("https://issuer.example.com")
///     .audience("https://api.example.com")
///     .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
///     .build()?;
/// # Ok::<(), adk_auth::sso::TokenError>(())
/// ```
#[cfg(feature = "sso")]
pub struct JwtValidator {
    /// Expected issuer.
    issuer: String,
    /// Expected audience; `None` only when the builder opted out with `without_audience`.
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
        match &self.audience {
            Some(aud) => {
                validation.set_audience(&[aud]);
                validation.set_required_spec_claims(&REQUIRED_CLAIMS);
            }
            // With no expected audience, jsonwebtoken refuses any token that carries `aud`.
            None => validation.set_required_spec_claims(&["exp", "iss"]),
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
    without_audience: bool,
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

    /// Set the expected audience: the value the issuer puts in the token's `aud` claim.
    ///
    /// The validator then refuses a token with no `aud` claim and one whose `aud` does not
    /// name this audience. [`build`](Self::build) requires an audience unless
    /// [`without_audience`](Self::without_audience) opts out.
    pub fn audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    /// Build the validator with no expected audience, for an issuer whose tokens carry no
    /// `aud` claim.
    ///
    /// The validator accepts a token with no `aud` claim and refuses one that carries any
    /// audience, so a token minted for another service is still refused. The `iss` claim
    /// stays required. A token that names no audience does not say which client it was
    /// minted for, so set [`audience`](Self::audience) whenever the issuer names one.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_auth::sso::JwtValidator;
    ///
    /// let validator = JwtValidator::builder()
    ///     .issuer("https://issuer.example.com")
    ///     .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
    ///     .without_audience()
    ///     .build()?;
    /// # Ok::<(), adk_auth::sso::TokenError>(())
    /// ```
    pub fn without_audience(mut self) -> Self {
        self.without_audience = true;
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
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::ValidationError`] when the issuer or JWKS URI is missing, when
    /// neither [`audience`](Self::audience) nor [`without_audience`](Self::without_audience)
    /// was called or both were, or when an algorithm is not an RSA or EC algorithm.
    pub fn build(self) -> Result<JwtValidator, TokenError> {
        let issuer =
            self.issuer.ok_or_else(|| TokenError::ValidationError("issuer is required".into()))?;
        let jwks_uri = self
            .jwks_uri
            .ok_or_else(|| TokenError::ValidationError("jwks_uri is required".into()))?;
        let audience = match (self.audience, self.without_audience) {
            (Some(audience), false) => Some(audience),
            (None, true) => None,
            (None, false) => {
                return Err(TokenError::ValidationError(
                    "audience is required. Set audience(...) to the value your issuer puts in the token's 'aud' claim, or call without_audience() for an issuer whose tokens carry no 'aud' claim."
                        .into(),
                ));
            }
            (Some(_), true) => {
                return Err(TokenError::ValidationError(
                    "audience(...) and without_audience() are mutually exclusive. Keep audience(...) to require the token's 'aud' claim."
                        .into(),
                ));
            }
        };

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
                // `jsonwebtoken::Algorithm` is `#[non_exhaustive]`, so a future release can add
                // an algorithm this validator has never vetted. Rejecting is the only safe
                // default: falling through would let an unreviewed algorithm validate tokens.
                unvetted => {
                    return Err(TokenError::ValidationError(format!(
                        "algorithm '{unvetted:?}' is not recognised by this validator. Use an RSA or EC algorithm instead."
                    )));
                }
            }
        }

        Ok(JwtValidator {
            issuer,
            audience,
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
    pub fn without_audience(self) -> Self {
        self
    }
    pub fn jwks_uri(self, _: impl Into<String>) -> Self {
        self
    }
    pub fn build(self) -> Result<JwtValidator, TokenError> {
        Err(TokenError::ValidationError("SSO feature not enabled".into()))
    }
}

#[cfg(all(test, feature = "sso"))]
mod tests {
    use super::{JwtValidator, JwtValidatorBuilder, TokenValidator};
    use crate::sso::TokenError;
    use crate::sso::test_issuer::{TestIssuer, assert_requires_iss_and_aud, token};

    const ISSUER: &str = "https://issuer.example.com";
    const AUDIENCE: &str = "https://api.example.com";

    fn builder(issuer: &TestIssuer) -> JwtValidatorBuilder {
        JwtValidator::builder().issuer(ISSUER).jwks_uri(issuer.jwks_uri())
    }

    /// `JwtValidator` is not `Debug`, so the error is taken by matching.
    fn build_error(builder: JwtValidatorBuilder) -> String {
        match builder.build() {
            Ok(_) => panic!("the builder should refuse this configuration"),
            Err(error) => error.to_string(),
        }
    }

    #[tokio::test]
    async fn build_requires_an_audience() {
        let issuer = TestIssuer::start().await;
        let error = build_error(builder(&issuer));
        assert!(error.contains("audience is required"), "{error}");
    }

    #[tokio::test]
    async fn a_pinned_audience_requires_the_iss_and_aud_claims() {
        let issuer = TestIssuer::start().await;
        let validator = builder(&issuer).audience(AUDIENCE).build().unwrap();

        assert_requires_iss_and_aud(&validator, ISSUER, AUDIENCE).await;
        let other_issuer = token(Some("https://evil.example"), Some(AUDIENCE));
        let refused = validator.validate(&other_issuer).await;
        assert!(matches!(&refused, Err(TokenError::ValidationError(_))), "{refused:?}");
    }

    #[tokio::test]
    async fn without_audience_refuses_any_aud_and_still_requires_iss() {
        let issuer = TestIssuer::start().await;
        let validator = builder(&issuer).without_audience().build().unwrap();

        let accepted = validator.validate(&token(Some(ISSUER), None)).await;
        assert!(accepted.as_ref().is_ok_and(|claims| claims.sub == "alice"), "{accepted:?}");
        let with_aud = validator.validate(&token(Some(ISSUER), Some(AUDIENCE))).await;
        assert!(matches!(&with_aud, Err(TokenError::ValidationError(_))), "{with_aud:?}");
        let no_iss = validator.validate(&token(None, None)).await;
        assert!(matches!(&no_iss, Err(TokenError::MissingClaim(c)) if c == "iss"), "{no_iss:?}");
    }

    #[tokio::test]
    async fn audience_and_without_audience_are_mutually_exclusive() {
        let issuer = TestIssuer::start().await;
        let error = build_error(builder(&issuer).audience(AUDIENCE).without_audience());
        assert!(error.contains("mutually exclusive"), "{error}");
    }
}
