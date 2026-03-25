//! Token validation errors.

use thiserror::Error;

/// Errors that can occur during token validation.
#[derive(Debug, Error)]
pub enum TokenError {
    /// Token has expired.
    #[error("Token expired")]
    Expired,

    /// Token is not yet valid (nbf claim).
    #[error("Token not yet valid")]
    NotYetValid,

    /// Invalid token signature.
    #[error("Invalid signature")]
    InvalidSignature,

    /// Token issuer doesn't match expected.
    #[error("Invalid issuer: expected '{expected}', got '{actual}'")]
    InvalidIssuer { expected: String, actual: String },

    /// Token audience doesn't match expected.
    #[error("Invalid audience: expected '{expected}', got '{actual:?}'")]
    InvalidAudience { expected: String, actual: Vec<String> },

    /// Required claim is missing.
    #[error("Missing required claim: {0}")]
    MissingClaim(String),

    /// Failed to fetch JWKS.
    #[error("JWKS fetch error: {0}")]
    JwksFetchError(String),

    /// Failed to parse JWKS.
    #[error("JWKS parse error: {0}")]
    JwksParseError(String),

    /// Key not found in JWKS.
    #[error("Key not found: kid={0}")]
    KeyNotFound(String),

    /// Token decoding failed.
    #[error("Decoding error: {0}")]
    DecodingError(String),

    /// Token format is invalid.
    #[error("Invalid token format: {0}")]
    InvalidFormat(String),

    /// Algorithm not supported.
    #[error("Unsupported algorithm: {0}")]
    UnsupportedAlgorithm(String),

    /// Discovery endpoint error.
    #[error("Discovery error: {0}")]
    DiscoveryError(String),

    /// Generic validation error.
    #[error("Validation error: {0}")]
    ValidationError(String),
}

#[cfg(feature = "sso")]
impl From<jsonwebtoken::errors::Error> for TokenError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        use jsonwebtoken::errors::ErrorKind;
        match err.kind() {
            ErrorKind::ExpiredSignature => TokenError::Expired,
            ErrorKind::ImmatureSignature => TokenError::NotYetValid,
            ErrorKind::InvalidSignature => TokenError::InvalidSignature,
            ErrorKind::InvalidAudience | ErrorKind::InvalidIssuer => {
                TokenError::ValidationError(err.to_string())
            }
            _ => TokenError::DecodingError(err.to_string()),
        }
    }
}

#[cfg(feature = "sso")]
impl From<reqwest::Error> for TokenError {
    fn from(err: reqwest::Error) -> Self {
        TokenError::JwksFetchError(err.to_string())
    }
}

impl From<TokenError> for adk_core::AdkError {
    fn from(err: TokenError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            TokenError::Expired => (ErrorCategory::Unauthorized, "auth.token_expired"),
            TokenError::NotYetValid => (ErrorCategory::Unauthorized, "auth.token_not_yet_valid"),
            TokenError::InvalidSignature => (ErrorCategory::Unauthorized, "auth.invalid_signature"),
            TokenError::InvalidIssuer { .. } => {
                (ErrorCategory::Unauthorized, "auth.invalid_issuer")
            }
            TokenError::InvalidAudience { .. } => {
                (ErrorCategory::Unauthorized, "auth.invalid_audience")
            }
            TokenError::MissingClaim(_) => (ErrorCategory::Unauthorized, "auth.missing_claim"),
            TokenError::JwksFetchError(_) => (ErrorCategory::Unavailable, "auth.jwks_fetch"),
            TokenError::JwksParseError(_) => (ErrorCategory::Internal, "auth.jwks_parse"),
            TokenError::KeyNotFound(_) => (ErrorCategory::NotFound, "auth.key_not_found"),
            TokenError::DecodingError(_) => (ErrorCategory::Unauthorized, "auth.decoding"),
            TokenError::InvalidFormat(_) => (ErrorCategory::InvalidInput, "auth.invalid_format"),
            TokenError::UnsupportedAlgorithm(_) => {
                (ErrorCategory::Unsupported, "auth.unsupported_algorithm")
            }
            TokenError::DiscoveryError(_) => (ErrorCategory::Unavailable, "auth.discovery"),
            TokenError::ValidationError(_) => (ErrorCategory::Unauthorized, "auth.validation"),
        };
        adk_core::AdkError::new(ErrorComponent::Auth, category, code, err.to_string())
            .with_source(err)
    }
}
