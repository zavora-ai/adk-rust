//! Errors of the phone (spec 3.3 §5.4).

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

use crate::error::SurfaceError;

/// Why a phone route, a push or a passkey check failed (spec 3.3 §5.4, §8.2).
///
/// Converts into [`AdkError`] with a `surface.phone.*` code, or the engine's own code for an
/// engine refusal; the phone's routes answer with its problem document and HTTP status.
///
/// # Example
///
/// ```rust
/// use adk_surface::mobile::MobileError;
///
/// let error = adk_core::AdkError::from(MobileError::NeedsHomeUrl);
/// assert_eq!(error.code, "surface.phone.needs_home_url");
/// assert_eq!(error.http_status_code(), 400);
/// ```
#[derive(Debug, thiserror::Error)]
pub enum MobileError {
    /// The server was started without the home's public URL, which push, passkeys and
    /// pairing links need.
    #[error(
        "the phone needs the home's public https URL: start adk-rust up with --home-url \
         https://<host>"
    )]
    NeedsHomeUrl,
    /// A push subscription points at a service the allowlist does not name, or not over
    /// https.
    #[error(
        "this push endpoint is refused: {reason}. A phone subscribes through its browser's push \
         service (Apple, Google, Mozilla or Microsoft) over https"
    )]
    PushEndpoint {
        /// Why.
        reason: String,
    },
    /// The push key could not be read, created or used.
    #[error(
        "the push key is unusable: {reason}. Delete the key file to create a new one; phones then \
         allow notifications again"
    )]
    PushKey {
        /// Why.
        reason: String,
    },
    /// Confirm it's you needs a passkey verifier this server does not have.
    #[error(
        "confirm it's you is not available on this server yet; approve high-risk calls on a \
         paired local terminal"
    )]
    PasskeyUnavailable,
    /// A passkey registration or assertion did not verify.
    #[error(
        "the passkey check failed: {reason}. Try again, or approve in the home on your computer"
    )]
    PasskeyRefused {
        /// Why, without secrets.
        reason: String,
    },
    /// A phone registers its passkey only shortly after pairing.
    #[error(
        "a phone sets up its passkey within 10 minutes of pairing: remove the phone and pair it \
         again"
    )]
    RegistrationClosed,
    /// The phone already has its passkey.
    #[error("this phone already has its passkey: remove the phone and pair it again to replace it")]
    PasskeyExists,
    /// The caller has no passkey to confirm with yet (spec 3.3 §8.2).
    #[error("{hint}")]
    NoPasskey {
        /// Where the caller sets one up, or approves instead.
        hint: &'static str,
    },
    /// The confirmation was not started here, was used, expired or belongs to another answer.
    #[error(
        "this confirmation is used, expired or for another answer: tap the answer again to start \
         a new one"
    )]
    Challenge,
    /// The QR code's URL is not one of this home's.
    #[error("a pairing QR code only holds a link into this home ({home})")]
    QrUrl {
        /// The home URL links must start with.
        home: String,
    },
    /// The engine refused or failed.
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    /// A store or the kernel failed.
    #[error(transparent)]
    Source(#[from] AdkError),
}

impl From<MobileError> for AdkError {
    fn from(error: MobileError) -> Self {
        let (category, code) = match &error {
            MobileError::Surface(_) | MobileError::Source(_) => {
                return match error {
                    MobileError::Surface(inner) => AdkError::from(inner),
                    MobileError::Source(inner) => inner,
                    _ => unreachable!("matched above"),
                };
            }
            MobileError::NeedsHomeUrl => {
                (ErrorCategory::InvalidInput, "surface.phone.needs_home_url")
            }
            MobileError::PushEndpoint { .. } => {
                (ErrorCategory::InvalidInput, "surface.phone.push_endpoint")
            }
            MobileError::PushKey { .. } => (ErrorCategory::Internal, "surface.phone.push_key"),
            MobileError::PasskeyUnavailable => {
                (ErrorCategory::Unsupported, "surface.phone.passkey_unavailable")
            }
            MobileError::PasskeyRefused { .. } => {
                (ErrorCategory::Forbidden, "surface.phone.passkey_refused")
            }
            MobileError::RegistrationClosed => {
                (ErrorCategory::Forbidden, "surface.phone.registration_closed")
            }
            MobileError::PasskeyExists => (ErrorCategory::Conflict, "surface.phone.passkey_exists"),
            MobileError::NoPasskey { .. } => (ErrorCategory::Forbidden, "surface.phone.no_passkey"),
            MobileError::Challenge => (ErrorCategory::Forbidden, "surface.phone.challenge"),
            MobileError::QrUrl { .. } => (ErrorCategory::InvalidInput, "surface.phone.qr_url"),
        };
        AdkError::new(ErrorComponent::Server, category, code, error.to_string())
    }
}
