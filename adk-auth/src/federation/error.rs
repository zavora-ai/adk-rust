//! Errors of agent identity federation.

use std::fmt;

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

use crate::sso::TokenError;

/// The cloud identity system an exchange or an inbound token belongs to.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::CloudProvider;
///
/// assert_eq!(CloudProvider::Entra.to_string(), "Microsoft Entra");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CloudProvider {
    /// AWS IAM and the AWS Security Token Service.
    Aws,
    /// Amazon Bedrock AgentCore Identity.
    AgentCore,
    /// Google Cloud workload identity federation.
    Google,
    /// Microsoft Entra Agent ID.
    Entra,
}

impl fmt::Display for CloudProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Aws => "AWS STS",
            Self::AgentCore => "Amazon Bedrock AgentCore Identity",
            Self::Google => "Google Cloud",
            Self::Entra => "Microsoft Entra",
        })
    }
}

/// Errors from issuing agent tokens, exchanging them at a cloud, and reading a cloud's
/// agent token.
///
/// Converts into [`AdkError`] with component `Auth` and an `identity.federation.*` code;
/// [`Token`](Self::Token) keeps the `auth.*` code of the [`TokenError`] it wraps.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{CloudProvider, FederationError};
///
/// let refused = FederationError::Refused {
///     provider: CloudProvider::Aws,
///     status: 403,
///     code: "AccessDenied".into(),
///     message: "Not authorized to perform sts:AssumeRoleWithWebIdentity".into(),
/// };
/// let error: adk_core::AdkError = refused.into();
/// assert_eq!(error.code, "identity.federation.exchange_refused");
/// assert_eq!(error.details.upstream_status_code, Some(403));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FederationError {
    /// An issuer or client setting is not usable.
    #[error("invalid federation configuration: {reason}")]
    InvalidConfig {
        /// What is wrong and how to correct it.
        reason: String,
    },
    /// The signing key was rejected.
    #[error(
        "invalid issuer signing key: {reason}; pass an unencrypted PKCS#8 RSA private key of 2048 bits or more (`openssl pkcs8 -topk8 -nocrypt` converts a PKCS#1 key)"
    )]
    InvalidKey {
        /// Why the key was rejected. Never holds key material.
        reason: String,
    },
    /// The signer could not sign a token.
    #[error(
        "signing the agent token failed: {reason}; check that the signing key or key service is available"
    )]
    Signing {
        /// What the signer reported.
        reason: String,
    },
    /// A request to a cloud endpoint did not complete.
    #[error(
        "the request to {provider} at '{endpoint}' failed: {reason}; check network access to the endpoint"
    )]
    Transport {
        /// The cloud asked.
        provider: CloudProvider,
        /// The endpoint asked.
        endpoint: String,
        /// The transport failure.
        reason: String,
    },
    /// The cloud answered and refused the exchange.
    #[error(
        "{provider} refused the exchange with HTTP {status} ({code}): {message}; check that the cloud trusts this issuer, subject and audience"
    )]
    Refused {
        /// The cloud that refused.
        provider: CloudProvider,
        /// The HTTP status of the refusal.
        status: u16,
        /// The cloud's error code, such as `AccessDenied` or `invalid_grant`.
        code: String,
        /// The cloud's error message, shortened to 512 characters.
        message: String,
    },
    /// The cloud accepted the exchange and its response could not be read.
    #[error("{provider} returned a response this client cannot read: {reason}")]
    InvalidResponse {
        /// The cloud that answered.
        provider: CloudProvider,
        /// What is missing or malformed.
        reason: String,
    },
    /// A credential broker was asked for a token it must not hand out.
    #[error("{provider} tokens cannot be requested for '{resource}': {reason}")]
    UnsupportedRequest {
        /// The cloud whose token was asked for.
        provider: CloudProvider,
        /// The resource the token was asked for.
        resource: String,
        /// Why the request is refused and what to ask for instead.
        reason: String,
    },
    /// An inbound token failed signature, issuer, audience or lifetime validation.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// A validated inbound token lacks a claim its profile needs to name the agent.
    #[error("the agent token has no usable `{claim}` claim: {reason}")]
    Claim {
        /// The claim name.
        claim: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl From<FederationError> for AdkError {
    fn from(err: FederationError) -> Self {
        let err = match err {
            // A validation failure keeps the `auth.*` code and category of its `TokenError`.
            FederationError::Token(token) => return token.into(),
            other => other,
        };
        let (category, code, status) = match &err {
            FederationError::InvalidConfig { .. } => {
                (ErrorCategory::InvalidInput, "identity.federation.invalid_config", None)
            }
            FederationError::InvalidKey { .. } => {
                (ErrorCategory::InvalidInput, "identity.federation.invalid_key", None)
            }
            FederationError::Signing { .. } => {
                (ErrorCategory::Internal, "identity.federation.signing_failed", None)
            }
            FederationError::Transport { .. } => {
                (ErrorCategory::Unavailable, "identity.federation.exchange_failed", None)
            }
            FederationError::Refused { status, .. } => {
                let category = match *status {
                    429 => ErrorCategory::RateLimited,
                    500..=599 => ErrorCategory::Unavailable,
                    403 => ErrorCategory::Forbidden,
                    _ => ErrorCategory::Unauthorized,
                };
                (category, "identity.federation.exchange_refused", Some(*status))
            }
            FederationError::InvalidResponse { .. } => {
                (ErrorCategory::Internal, "identity.federation.invalid_response", None)
            }
            FederationError::UnsupportedRequest { .. } => {
                (ErrorCategory::Forbidden, "identity.federation.request_unsupported", None)
            }
            FederationError::Token(_) | FederationError::Claim { .. } => {
                (ErrorCategory::Unauthorized, "identity.federation.claim_invalid", None)
            }
        };
        let error = AdkError::new(ErrorComponent::Auth, category, code, err.to_string());
        match status {
            Some(status) => error.with_upstream_status(status).with_source(err),
            None => error.with_source(err),
        }
    }
}
