//! Errors from Web Bot Auth signing and verification. Spec 3.5 §7.3.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// Why a Web Bot Auth signature could not be produced or verified.
///
/// [`is_unverified`](Self::is_unverified) separates the draft's two failure outcomes
/// (draft-ietf-webbotauth-httpsig-protocol-00 Appendix C.1): an *invalid* signature failed a
/// check, while an *unverified* one could not be checked because its key material was not
/// available. Neither grants an identity. Spec 3.5 §7.3, A-I6.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::WebBotAuthError;
///
/// let err = WebBotAuthError::UnknownKey { origin: "https://bot.example".into(), keyid: "k1".into() };
/// assert!(err.is_unverified());
/// let adk: adk_core::AdkError = err.into();
/// assert!(adk.is_unauthorized());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WebBotAuthError {
    /// A `Signature`, `Signature-Input` or `Signature-Agent` field is not a valid structured field.
    #[error(
        "malformed `{field}` field: {reason}; send it as an RFC 8941 Dictionary as RFC 9421 §4 and the Web Bot Auth draft define"
    )]
    MalformedField {
        /// The field name.
        field: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A signature parameter is missing, has the wrong type, or contradicts another.
    #[error(
        "signature `{label}` has an invalid `{parameter}` parameter: {reason}; Web Bot Auth signatures carry created, expires, keyid and tag=\"web-bot-auth\""
    )]
    InvalidParameter {
        /// The signature label.
        label: String,
        /// The parameter name.
        parameter: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The signature does not cover a component the verifier requires.
    #[error(
        "signature `{label}` does not cover `{component}`; sign it, as the Web Bot Auth draft §5.2 and the verifier's policy require"
    )]
    MissingComponent {
        /// The signature label.
        label: String,
        /// The required component.
        component: String,
    },
    /// The signature names an algorithm other than Ed25519.
    #[error("signature `{label}` uses algorithm `{alg}`; only alg=\"ed25519\" is accepted")]
    UnsupportedAlgorithm {
        /// The signature label.
        label: String,
        /// The algorithm it names.
        alg: String,
    },
    /// The signature's `created` time is in the future beyond the allowed clock skew.
    #[error(
        "signature `{label}` was created at {created}, after the verifier's time {now} plus the allowed skew; check the signer's clock"
    )]
    NotYetValid {
        /// The signature label.
        label: String,
        /// The `created` parameter.
        created: i64,
        /// The verifier's time.
        now: i64,
    },
    /// The signature's `expires` time has passed beyond the allowed clock skew.
    #[error("signature `{label}` expired at {expires} (now {now}); sign each request afresh")]
    Expired {
        /// The signature label.
        label: String,
        /// The `expires` parameter.
        expires: i64,
        /// The verifier's time.
        now: i64,
    },
    /// The signature is valid for longer than the verifier accepts.
    #[error(
        "signature `{label}` is valid for {lifetime_secs} s, more than the {max_secs} s the verifier accepts; use a shorter expiry"
    )]
    LifetimeTooLong {
        /// The signature label.
        label: String,
        /// `expires - created`.
        lifetime_secs: i64,
        /// The verifier's limit.
        max_secs: u64,
    },
    /// The `Signature-Agent` member the signature covers is missing or is not an origin.
    #[error(
        "signature `{label}` has an unusable Signature-Agent: {reason}; send an https origin such as \"https://bot.example\""
    )]
    InvalidSignatureAgent {
        /// The signature label.
        label: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The `Signature-Agent` member names a discovery type the verifier does not support.
    #[error(
        "signature `{label}` uses Signature-Agent discovery type `{kind}`; only the `directory` type is supported"
    )]
    UnsupportedDiscovery {
        /// The signature label.
        label: String,
        /// The discovery type.
        kind: String,
    },
    /// The signature base cannot be built from the message (RFC 9421 §2.5).
    #[error("cannot build the signature base: {reason}")]
    SignatureBase {
        /// What is missing or malformed.
        reason: String,
    },
    /// The signature does not verify against the key.
    #[error(
        "signature `{label}` does not verify against its key; the message changed or the key is wrong"
    )]
    BadSignature {
        /// The signature label.
        label: String,
    },
    /// The key directory does not contain the key the signature names.
    #[error(
        "the key directory at {origin} has no Ed25519 key with thumbprint `{keyid}`; publish the key before signing with it"
    )]
    UnknownKey {
        /// The directory origin.
        origin: String,
        /// The `keyid` parameter.
        keyid: String,
    },
    /// The key exists but its `nbf`/`exp` window does not include the current time.
    #[error("key `{keyid}` at {origin} is outside its validity window")]
    KeyNotCurrent {
        /// The directory origin.
        origin: String,
        /// The key thumbprint.
        keyid: String,
    },
    /// The key is a published test key (draft §6.8).
    #[error(
        "key `{keyid}` is the RFC 9421 test key, which verifiers reject in production; generate a key of your own"
    )]
    TestKey {
        /// The key thumbprint.
        keyid: String,
    },
    /// The key directory could not be fetched.
    #[error("the key directory at {origin} is unavailable: {reason}")]
    DirectoryUnavailable {
        /// The directory origin.
        origin: String,
        /// Why the fetch failed.
        reason: String,
    },
    /// The key directory was fetched but is not a valid directory.
    #[error(
        "the key directory at {origin} is malformed: {reason}; serve a JWKS as application/http-message-signatures-directory+json"
    )]
    MalformedDirectory {
        /// The directory origin.
        origin: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The directory fetch would reach a non-public address or use a refused scheme.
    #[error("refused to fetch the key directory at {origin}: {reason}")]
    EgressRefused {
        /// The directory origin.
        origin: String,
        /// Why the fetch was refused.
        reason: String,
    },
    /// Key material is malformed.
    #[error("invalid key material: {reason}")]
    InvalidKey {
        /// What is wrong with it.
        reason: String,
    },
    /// A signer, verifier or source was configured with invalid values.
    #[error("invalid Web Bot Auth configuration: {reason}")]
    InvalidConfig {
        /// What is wrong with it.
        reason: String,
    },
}

impl WebBotAuthError {
    /// Whether the signature could not be checked rather than failed a check: its key material
    /// was unavailable, so the request is *unverified* (draft Appendix C.1, §6.10).
    pub fn is_unverified(&self) -> bool {
        match self {
            Self::UnknownKey { .. }
            | Self::UnsupportedDiscovery { .. }
            | Self::DirectoryUnavailable { .. }
            | Self::MalformedDirectory { .. }
            | Self::EgressRefused { .. } => true,
            Self::MalformedField { .. }
            | Self::InvalidParameter { .. }
            | Self::MissingComponent { .. }
            | Self::UnsupportedAlgorithm { .. }
            | Self::NotYetValid { .. }
            | Self::Expired { .. }
            | Self::LifetimeTooLong { .. }
            | Self::InvalidSignatureAgent { .. }
            | Self::SignatureBase { .. }
            | Self::BadSignature { .. }
            | Self::KeyNotCurrent { .. }
            | Self::TestKey { .. }
            | Self::InvalidKey { .. }
            | Self::InvalidConfig { .. } => false,
        }
    }

    fn category_and_code(&self) -> (ErrorCategory, &'static str) {
        match self {
            Self::MalformedField { .. } => {
                (ErrorCategory::InvalidInput, "auth.web_bot_auth.malformed_field")
            }
            Self::InvalidParameter { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.invalid_parameter")
            }
            Self::MissingComponent { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.missing_component")
            }
            Self::UnsupportedAlgorithm { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.unsupported_algorithm")
            }
            Self::NotYetValid { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.not_yet_valid")
            }
            Self::Expired { .. } => (ErrorCategory::Unauthorized, "auth.web_bot_auth.expired"),
            Self::LifetimeTooLong { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.lifetime_too_long")
            }
            Self::InvalidSignatureAgent { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.invalid_signature_agent")
            }
            Self::UnsupportedDiscovery { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.unsupported_discovery")
            }
            Self::SignatureBase { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.signature_base")
            }
            Self::BadSignature { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.bad_signature")
            }
            Self::UnknownKey { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.unknown_key")
            }
            Self::KeyNotCurrent { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.key_not_current")
            }
            Self::TestKey { .. } => (ErrorCategory::Unauthorized, "auth.web_bot_auth.test_key"),
            Self::DirectoryUnavailable { .. } => {
                (ErrorCategory::Unavailable, "auth.web_bot_auth.directory_unavailable")
            }
            Self::MalformedDirectory { .. } => {
                (ErrorCategory::Unauthorized, "auth.web_bot_auth.malformed_directory")
            }
            Self::EgressRefused { .. } => {
                (ErrorCategory::Forbidden, "auth.web_bot_auth.egress_refused")
            }
            Self::InvalidKey { .. } => {
                (ErrorCategory::InvalidInput, "auth.web_bot_auth.invalid_key")
            }
            Self::InvalidConfig { .. } => {
                (ErrorCategory::InvalidInput, "auth.web_bot_auth.invalid_config")
            }
        }
    }
}

impl From<WebBotAuthError> for AdkError {
    fn from(err: WebBotAuthError) -> Self {
        let (category, code) = err.category_and_code();
        AdkError::new(ErrorComponent::Auth, category, code, err.to_string()).with_source(err)
    }
}
