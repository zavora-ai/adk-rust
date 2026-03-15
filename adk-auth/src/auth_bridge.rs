//! JWT request context extractor for `adk-server`.
//!
//! This module provides a reusable [`JwtRequestContextExtractor`] implementation
//! for the `adk-server` auth bridge. It validates Bearer tokens, maps the user
//! identity, and extracts scope claims into [`adk_core::RequestContext`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::auth_bridge::JwtRequestContextExtractor;
//! use adk_auth::sso::{ClaimsMapper, GoogleProvider};
//!
//! let extractor = JwtRequestContextExtractor::builder()
//!     .validator(GoogleProvider::new("client-id"))
//!     .mapper(ClaimsMapper::builder().user_id_from_email().build())
//!     .build()?;
//! ```

use crate::sso::{ClaimsMapper, TokenClaims, TokenValidator};
use adk_core::UserId;
use adk_server::auth_bridge::{RequestContext, RequestContextError, RequestContextExtractor};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Validates a Bearer token and converts it into an `adk-core` request context.
pub struct JwtRequestContextExtractor {
    validator: Arc<dyn TokenValidator>,
    mapper: ClaimsMapper,
}

impl JwtRequestContextExtractor {
    /// Create a new builder.
    pub fn builder() -> JwtRequestContextExtractorBuilder {
        JwtRequestContextExtractorBuilder::default()
    }

    fn bearer_token<'a>(
        &self,
        parts: &'a axum::http::request::Parts,
    ) -> Result<&'a str, RequestContextError> {
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;

        auth_header.strip_prefix("Bearer ").ok_or_else(|| {
            RequestContextError::InvalidToken("expected Bearer authorization scheme".into())
        })
    }

    fn request_context_from_claims(
        &self,
        claims: &TokenClaims,
    ) -> Result<RequestContext, RequestContextError> {
        let mut metadata = HashMap::new();
        metadata.insert("issuer".to_string(), claims.iss.clone());
        metadata.insert("subject".to_string(), claims.sub.clone());
        if let Some(email) = &claims.email {
            metadata.insert("email".to_string(), email.clone());
        }
        if let Some(tenant_id) = &claims.tid {
            metadata.insert("tenant_id".to_string(), tenant_id.clone());
        }
        if let Some(hosted_domain) = &claims.hd {
            metadata.insert("hosted_domain".to_string(), hosted_domain.clone());
        }

        let user_id = self.mapper.get_user_id(claims);
        let validated_user_id = UserId::try_from(user_id.as_str()).map_err(|err| {
            RequestContextError::ExtractionFailed(format!("invalid mapped user id: {err}"))
        })?;

        Ok(RequestContext {
            user_id: validated_user_id.to_string(),
            scopes: claims.scopes(),
            metadata,
        })
    }
}

#[async_trait]
impl RequestContextExtractor for JwtRequestContextExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let token = self.bearer_token(parts)?;
        let claims = self
            .validator
            .validate(token)
            .await
            .map_err(|err| RequestContextError::InvalidToken(err.to_string()))?;

        self.request_context_from_claims(&claims)
    }
}

/// Builder for [`JwtRequestContextExtractor`].
#[derive(Default)]
pub struct JwtRequestContextExtractorBuilder {
    validator: Option<Arc<dyn TokenValidator>>,
    mapper: Option<ClaimsMapper>,
}

impl JwtRequestContextExtractorBuilder {
    /// Set the token validator used to authenticate Bearer tokens.
    pub fn validator(mut self, validator: impl TokenValidator + 'static) -> Self {
        self.validator = Some(Arc::new(validator));
        self
    }

    /// Set the claims mapper used to select the request `user_id`.
    pub fn mapper(mut self, mapper: ClaimsMapper) -> Self {
        self.mapper = Some(mapper);
        self
    }

    /// Build the extractor.
    pub fn build(self) -> Result<JwtRequestContextExtractor, &'static str> {
        Ok(JwtRequestContextExtractor {
            validator: self.validator.ok_or("validator is required")?,
            mapper: self.mapper.unwrap_or_else(|| ClaimsMapper::builder().build()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sso::{TokenError, TokenValidator};

    struct StaticValidator {
        claims: TokenClaims,
    }

    #[async_trait]
    impl TokenValidator for StaticValidator {
        async fn validate(&self, token: &str) -> Result<TokenClaims, TokenError> {
            if token == "valid-token" {
                Ok(self.claims.clone())
            } else {
                Err(TokenError::ValidationError("unknown token".into()))
            }
        }

        fn issuer(&self) -> &str {
            "https://issuer.example.com"
        }
    }

    #[tokio::test]
    async fn test_extract_maps_verified_email_and_scopes() {
        let mut claims = TokenClaims {
            sub: "user-123".into(),
            iss: "https://issuer.example.com".into(),
            email: Some("alice@example.com".into()),
            email_verified: Some(true),
            tid: Some("tenant-1".into()),
            ..Default::default()
        };
        claims.custom.insert("scope".into(), serde_json::json!("read write"));
        claims.custom.insert("scp".into(), serde_json::json!(["write", "admin"]));

        let extractor = JwtRequestContextExtractor::builder()
            .validator(StaticValidator { claims })
            .mapper(ClaimsMapper::builder().user_id_from_email().build())
            .build()
            .unwrap();

        let request = axum::http::Request::builder()
            .header("authorization", "Bearer valid-token")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        let context = extractor.extract(&parts).await.unwrap();
        assert_eq!(context.user_id, "alice@example.com");
        assert_eq!(context.scopes, vec!["read", "write", "admin"]);
        assert_eq!(context.metadata.get("issuer"), Some(&"https://issuer.example.com".to_string()));
        assert_eq!(context.metadata.get("tenant_id"), Some(&"tenant-1".to_string()));
    }

    #[tokio::test]
    async fn test_extract_requires_bearer_scheme() {
        let extractor = JwtRequestContextExtractor::builder()
            .validator(StaticValidator { claims: TokenClaims::default() })
            .build()
            .unwrap();

        let request = axum::http::Request::builder()
            .header("authorization", "Basic invalid")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        assert!(matches!(
            extractor.extract(&parts).await.unwrap_err(),
            RequestContextError::InvalidToken(_)
        ));
    }

    #[tokio::test]
    async fn test_extract_rejects_invalid_mapped_user_id() {
        let claims = TokenClaims {
            sub: String::new(),
            iss: "https://issuer.example.com".into(),
            ..Default::default()
        };

        let extractor = JwtRequestContextExtractor::builder()
            .validator(StaticValidator { claims })
            .mapper(ClaimsMapper::builder().user_id_from_sub().build())
            .build()
            .unwrap();

        let request = axum::http::Request::builder()
            .header("authorization", "Bearer valid-token")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        assert!(matches!(
            extractor.extract(&parts).await.unwrap_err(),
            RequestContextError::ExtractionFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_extract_rejects_null_byte_in_mapped_user_id() {
        let claims = TokenClaims {
            sub: "bad\0user".into(),
            iss: "https://issuer.example.com".into(),
            ..Default::default()
        };

        let extractor = JwtRequestContextExtractor::builder()
            .validator(StaticValidator { claims })
            .mapper(ClaimsMapper::builder().user_id_from_sub().build())
            .build()
            .unwrap();

        let request = axum::http::Request::builder()
            .header("authorization", "Bearer valid-token")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        assert!(matches!(
            extractor.extract(&parts).await.unwrap_err(),
            RequestContextError::ExtractionFailed(_)
        ));
    }
}
