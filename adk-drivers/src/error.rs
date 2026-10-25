//! The drivers' error type (spec 3.5 §7.13).

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// Why a driver call did not happen (spec 3.5 §7.13).
///
/// Converts into [`AdkError`] with a `drivers.*` code, so a tool call that a rule refused
/// reaches the model as that rule's reason. Never carries a credential.
///
/// # Example
///
/// ```rust
/// use adk_drivers::DriverError;
///
/// let error = DriverError::refused("the recipient unsubscribed");
/// assert_eq!(error.code(), "drivers.refused");
/// let adk: adk_core::AdkError = error.into();
/// assert_eq!(adk.category, adk_core::ErrorCategory::Forbidden);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DriverError {
    /// The call's input is not valid for the contract.
    #[error("invalid driver call: {reason}")]
    Invalid {
        /// What is wrong, and how to fix it.
        reason: String,
    },
    /// A rule of the contract refused the call: an unsubscribed recipient, a missing
    /// consent receipt, failing domain authentication, a new recipient in a reply.
    #[error("refused: {reason}")]
    Refused {
        /// Which rule refused it.
        reason: String,
    },
    /// Too many calls of this kind for now.
    #[error("rate limited: {reason}; try again in {retry_after_secs} s")]
    RateLimited {
        /// What is limited.
        reason: String,
        /// How long to wait.
        retry_after_secs: u64,
    },
    /// The driver does not offer this operation.
    #[error("{driver} does not support {operation}; use a driver that does")]
    Unsupported {
        /// The driver.
        driver: String,
        /// The operation.
        operation: String,
    },
    /// The thing the call names does not exist.
    #[error("not found: {what}")]
    NotFound {
        /// What was not found.
        what: String,
    },
    /// The service refused the driver's credentials.
    #[error(
        "{service} refused the driver's credentials: {reason}; check the secret the credential broker holds for it"
    )]
    Unauthorized {
        /// The service.
        service: String,
        /// What it said.
        reason: String,
    },
    /// The service could not be reached or failed.
    #[error("{service} is unavailable: {reason}")]
    Unavailable {
        /// The service.
        service: String,
        /// What failed.
        reason: String,
    },
    /// The service answered with something the driver could not read.
    #[error("{service} answered with something unreadable: {reason}")]
    Protocol {
        /// The service.
        service: String,
        /// What was wrong.
        reason: String,
    },
    /// A recorded-response driver has no recorded answer for the call.
    #[error("no recorded response for {request}; record one or use a live adapter")]
    NoRecording {
        /// The call, as the recording keys it.
        request: String,
    },
}

impl DriverError {
    /// An [`Invalid`](Self::Invalid) error.
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self::Invalid { reason: reason.into() }
    }

    /// A [`Refused`](Self::Refused) error.
    pub fn refused(reason: impl Into<String>) -> Self {
        Self::Refused { reason: reason.into() }
    }

    /// An [`Unavailable`](Self::Unavailable) error.
    pub fn unavailable(service: impl Into<String>, reason: impl ToString) -> Self {
        Self::Unavailable { service: service.into(), reason: reason.to_string() }
    }

    /// A [`Protocol`](Self::Protocol) error.
    pub fn protocol(service: impl Into<String>, reason: impl ToString) -> Self {
        Self::Protocol { service: service.into(), reason: reason.to_string() }
    }

    /// A [`NotFound`](Self::NotFound) error.
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound { what: what.into() }
    }

    /// An [`Unsupported`](Self::Unsupported) error.
    pub fn unsupported(driver: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::Unsupported { driver: driver.into(), operation: operation.into() }
    }

    /// The error code: `drivers.invalid`, `.refused`, `.rate_limited`, `.unsupported`,
    /// `.not_found`, `.unauthorized`, `.unavailable`, `.protocol` or `.no_recording`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid { .. } => "drivers.invalid",
            Self::Refused { .. } => "drivers.refused",
            Self::RateLimited { .. } => "drivers.rate_limited",
            Self::Unsupported { .. } => "drivers.unsupported",
            Self::NotFound { .. } => "drivers.not_found",
            Self::Unauthorized { .. } => "drivers.unauthorized",
            Self::Unavailable { .. } => "drivers.unavailable",
            Self::Protocol { .. } => "drivers.protocol",
            Self::NoRecording { .. } => "drivers.no_recording",
        }
    }

    fn category(&self) -> ErrorCategory {
        match self {
            Self::Invalid { .. } => ErrorCategory::InvalidInput,
            Self::Refused { .. } => ErrorCategory::Forbidden,
            Self::RateLimited { .. } => ErrorCategory::RateLimited,
            Self::Unsupported { .. } => ErrorCategory::Unsupported,
            Self::NotFound { .. } => ErrorCategory::NotFound,
            Self::Unauthorized { .. } => ErrorCategory::Unauthorized,
            Self::Unavailable { .. } => ErrorCategory::Unavailable,
            Self::Protocol { .. } | Self::NoRecording { .. } => ErrorCategory::Internal,
        }
    }
}

impl From<DriverError> for AdkError {
    fn from(error: DriverError) -> Self {
        AdkError::new(ErrorComponent::Tool, error.category(), error.code(), error.to_string())
    }
}

impl From<DriverError> for adk_kernel::KernelError {
    fn from(error: DriverError) -> Self {
        Self::Core(error.into())
    }
}

/// The result of a driver call.
pub type Result<T> = std::result::Result<T, DriverError>;
