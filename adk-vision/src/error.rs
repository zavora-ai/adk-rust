//! [`VisionError`], the crate's error type, and its mapping to [`AdkError`].

use std::path::PathBuf;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, RetryHint};

use crate::digest::Digest;

/// What went wrong in a vision operation.
///
/// Every variant maps to an [`AdkError`] category at the boundary (`From<VisionError>`):
/// an unreleased image or a refused egress to `Forbidden`, a licence gate to `Unauthorized`
/// with the acceptance command in the message, a provider's rate limit to `RateLimited`
/// with its retry hint.
///
/// # Example
///
/// ```
/// use adk_core::{AdkError, ErrorCategory};
/// use adk_vision::VisionError;
///
/// let err = VisionError::TooManyPixels { limit: 40_000_000, actual: 64_000_000 };
/// let adk: AdkError = err.into();
/// assert_eq!(adk.category, ErrorCategory::InvalidInput);
/// assert!(adk.message.contains("40000000"));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VisionError {
    /// The bytes are not one of the accepted image formats.
    #[error(
        "unsupported image format {detected}; accepted formats are PNG, JPEG, WebP and the first frame of a GIF"
    )]
    UnsupportedFormat {
        /// The format the header named, or `unknown`.
        detected: String,
    },

    /// The encoded image is larger than the byte cap.
    #[error(
        "the image is {actual} bytes, over the {limit}-byte cap; downscale or recompress it before handing it over"
    )]
    TooManyBytes {
        /// The cap in bytes.
        limit: u64,
        /// The image's size in bytes.
        actual: u64,
    },

    /// The image has more pixels than the cap, read from its header before decoding.
    #[error(
        "the image has {actual} pixels, over the {limit}-pixel cap; downscale it before handing it over"
    )]
    TooManyPixels {
        /// The cap in pixels.
        limit: u64,
        /// The image's pixel count.
        actual: u64,
    },

    /// The image could not be decoded.
    #[error("the image could not be decoded: {0}")]
    Decode(String),

    /// The image exceeds what a backend accepts and the backend refuses to have it resized.
    #[error(
        "backend `{backend}` accepts at most {limit}; the image is {actual}. Crop or downscale it first"
    )]
    InputTooLarge {
        /// The backend that refused the input.
        backend: String,
        /// The limit, in words (`2576 px on the long edge`).
        limit: String,
        /// The input's size, in the same words.
        actual: String,
    },

    /// An image was routed to a remote backend without passing the egress gate.
    #[error(
        "backend `{backend}` is remote and only takes images released by the egress gate; configure an `EgressGate` on the runtime"
    )]
    NotReleased {
        /// The remote backend.
        backend: String,
    },

    /// The egress gate refused to release an image.
    #[error("the image was not released to {host}: {reason}")]
    EgressRefused {
        /// The destination host.
        host: String,
        /// Why, in words a person can act on.
        reason: String,
    },

    /// The egress rule for this destination and data class asks a person first.
    #[error(
        "sending image {egress} to {host} as `{data_class}` needs a person's approval; approve that digest and retry"
    )]
    EgressHeld {
        /// The destination host.
        host: String,
        /// The data class of the image.
        data_class: String,
        /// The digest of the exact redacted image that would be sent.
        egress: Digest,
    },

    /// The policy requires a redaction class for which no redactor is installed.
    #[error(
        "the egress policy requires `{class}` redaction and no redactor for it is installed; add one to the egress gate or remove the class from the policy"
    )]
    RedactionUnavailable {
        /// The redaction class with no redactor.
        class: String,
    },

    /// A use-restricted model was requested before a person accepted its licence.
    #[error(
        "licence acceptance required for model `{model}`: read {licence_url}, then run `adk-rust vision models accept {model}`"
    )]
    LicenceAcceptanceRequired {
        /// The model manifest id.
        model: String,
        /// The licence, or the gated repository page, a person must read.
        licence_url: String,
    },

    /// A non-commercial or copyleft model that the configuration does not name.
    #[error(
        "model `{model}` is licensed {class} ({licence_url}); name it in the licence policy to allow it"
    )]
    LicenceNotAllowed {
        /// The model manifest id.
        model: String,
        /// The licence class.
        class: String,
        /// The licence URL.
        licence_url: String,
    },

    /// A model file's SHA-256 differs from its manifest; the file is never loaded.
    #[error(
        "model file {path} does not match its manifest: expected {expected}, found {actual}; delete it and fetch again"
    )]
    DigestMismatch {
        /// The file on disk.
        path: PathBuf,
        /// The digest the manifest pins.
        expected: Digest,
        /// The digest of the file on disk.
        actual: Digest,
    },

    /// A model file's size differs from its manifest; the file is never loaded.
    #[error(
        "model file {path} is {actual} bytes, but its manifest pins {expected}; delete it and fetch again"
    )]
    SizeMismatch {
        /// The file on disk.
        path: PathBuf,
        /// The size the manifest pins.
        expected: u64,
        /// The size on disk.
        actual: u64,
    },

    /// Offline mode is on and a file is not in the cache.
    #[error(
        "`{file}` of model `{model}` is not in the cache and HF_HUB_OFFLINE is set; fetch it once with network access"
    )]
    Offline {
        /// The model manifest id.
        model: String,
        /// The missing file.
        file: String,
    },

    /// A download failed.
    #[error("fetching `{file}` of model `{model}` failed: {message}")]
    Fetch {
        /// The model manifest id.
        model: String,
        /// The file being fetched.
        file: String,
        /// What failed.
        message: String,
    },

    /// A manifest is invalid.
    #[error("model manifest `{id}` is invalid: {reason}")]
    Manifest {
        /// The manifest id.
        id: String,
        /// Why.
        reason: String,
    },

    /// A provider refused for rate limiting.
    #[error("backend `{backend}` is rate limited; retry later")]
    RateLimited {
        /// The backend.
        backend: String,
        /// The provider's retry hint.
        retry_after: Option<Duration>,
    },

    /// A backend call ran out of time.
    #[error("backend `{backend}` timed out")]
    Timeout {
        /// The backend.
        backend: String,
    },

    /// The call was cancelled.
    #[error("the vision call was cancelled")]
    Cancelled,

    /// A backend is temporarily unavailable.
    #[error("backend `{backend}` is unavailable: {message}")]
    Unavailable {
        /// The backend.
        backend: String,
        /// What failed.
        message: String,
    },

    /// A capability a backend or the runtime does not have.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The caller's input is invalid.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// No backend with this id is registered, or none serves the task.
    #[error("no vision backend `{0}` is registered for this task")]
    BackendNotFound(String),

    /// A backend's output could not be parsed; nothing is guessed.
    #[error("backend `{backend}` returned output that could not be parsed: {message}")]
    Parse {
        /// The backend.
        backend: String,
        /// What was wrong.
        message: String,
    },

    /// A backend failed.
    #[error("backend `{backend}` failed: {message}")]
    Backend {
        /// The backend.
        backend: String,
        /// What failed.
        message: String,
    },

    /// A file operation failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The path.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A visual memory collection holds vectors of another embedding space; vectors of two
    /// spaces never share a collection.
    #[error(
        "collection `{collection}` holds vectors of embedding space {recorded}, not {offered}; \
         re-index into a new collection with this embedder, or use the embedder the collection \
         was built with"
    )]
    SpaceMismatch {
        /// The collection.
        collection: String,
        /// The space the collection recorded, or a note that its record could not be read.
        recorded: String,
        /// The space of the embedder that was offered.
        offered: String,
    },

    /// An error from the platform: a budget or spend refusal, a frozen kill switch, storage.
    #[error(transparent)]
    Core(#[from] AdkError),
}

impl VisionError {
    /// The stable error code, `vision.<kind>`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedFormat { .. } => "vision.unsupported_format",
            Self::TooManyBytes { .. } => "vision.too_many_bytes",
            Self::TooManyPixels { .. } => "vision.too_many_pixels",
            Self::Decode(_) => "vision.decode",
            Self::InputTooLarge { .. } => "vision.input_too_large",
            Self::NotReleased { .. } => "vision.not_released",
            Self::EgressRefused { .. } => "vision.egress.refused",
            Self::EgressHeld { .. } => "vision.egress.held",
            Self::RedactionUnavailable { .. } => "vision.egress.redaction_unavailable",
            Self::LicenceAcceptanceRequired { .. } => "vision.model.licence_acceptance_required",
            Self::LicenceNotAllowed { .. } => "vision.model.licence_not_allowed",
            Self::DigestMismatch { .. } => "vision.model.digest_mismatch",
            Self::SizeMismatch { .. } => "vision.model.size_mismatch",
            Self::Offline { .. } => "vision.model.offline",
            Self::Fetch { .. } => "vision.model.fetch",
            Self::Manifest { .. } => "vision.model.manifest",
            Self::RateLimited { .. } => "vision.rate_limited",
            Self::Timeout { .. } => "vision.timeout",
            Self::Cancelled => "vision.cancelled",
            Self::Unavailable { .. } => "vision.unavailable",
            Self::Unsupported(_) => "vision.unsupported",
            Self::InvalidInput(_) => "vision.invalid_input",
            Self::BackendNotFound(_) => "vision.backend_not_found",
            Self::Parse { .. } => "vision.parse",
            Self::Backend { .. } => "vision.backend",
            Self::Io { .. } => "vision.io",
            Self::SpaceMismatch { .. } => "vision.memory.space_mismatch",
            Self::Core(_) => "vision.core",
        }
    }

    /// The [`ErrorCategory`] this error maps to.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::UnsupportedFormat { .. }
            | Self::TooManyBytes { .. }
            | Self::TooManyPixels { .. }
            | Self::Decode(_)
            | Self::InputTooLarge { .. }
            | Self::Manifest { .. }
            | Self::SpaceMismatch { .. }
            | Self::InvalidInput(_) => ErrorCategory::InvalidInput,
            Self::NotReleased { .. }
            | Self::EgressRefused { .. }
            | Self::EgressHeld { .. }
            | Self::RedactionUnavailable { .. }
            | Self::LicenceNotAllowed { .. }
            | Self::DigestMismatch { .. }
            | Self::SizeMismatch { .. } => ErrorCategory::Forbidden,
            Self::LicenceAcceptanceRequired { .. } => ErrorCategory::Unauthorized,
            Self::Offline { .. } | Self::BackendNotFound(_) => ErrorCategory::NotFound,
            Self::Fetch { .. } | Self::Unavailable { .. } => ErrorCategory::Unavailable,
            Self::RateLimited { .. } => ErrorCategory::RateLimited,
            Self::Timeout { .. } => ErrorCategory::Timeout,
            Self::Cancelled => ErrorCategory::Cancelled,
            Self::Unsupported(_) => ErrorCategory::Unsupported,
            Self::Parse { .. } | Self::Backend { .. } | Self::Io { .. } => ErrorCategory::Internal,
            Self::Core(err) => err.category,
        }
    }

    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io { action, path: path.into(), source }
    }
}

impl From<VisionError> for AdkError {
    fn from(err: VisionError) -> Self {
        let code = err.code();
        let category = err.category();
        match err {
            VisionError::Core(inner) => inner,
            VisionError::RateLimited { ref backend, retry_after } => {
                let mut retry = RetryHint::for_category(category);
                if let Some(after) = retry_after {
                    retry = retry.with_retry_after(after);
                }
                let provider = backend.clone();
                AdkError::new(ErrorComponent::Tool, category, code, err.to_string())
                    .with_retry(retry)
                    .with_provider(provider)
            }
            other => AdkError::new(ErrorComponent::Tool, category, code, other.to_string()),
        }
    }
}

/// `Result` with [`VisionError`].
pub type Result<T, E = VisionError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_carry_the_retry_hint() {
        let err: AdkError = VisionError::RateLimited {
            backend: "sam-3.1".into(),
            retry_after: Some(Duration::from_secs(3)),
        }
        .into();
        assert_eq!(err.category, ErrorCategory::RateLimited);
        assert_eq!(err.code, "vision.rate_limited");
        assert_eq!(err.retry.retry_after(), Some(Duration::from_secs(3)));
        assert!(err.retry.should_retry);
    }

    #[test]
    fn licence_gate_names_the_url_and_the_command() {
        let err: AdkError = VisionError::LicenceAcceptanceRequired {
            model: "sam3-local".into(),
            licence_url: "https://example.org/sam-license".into(),
        }
        .into();
        assert_eq!(err.category, ErrorCategory::Unauthorized);
        assert!(err.message.contains("licence acceptance required"));
        assert!(err.message.contains("https://example.org/sam-license"));
        assert!(err.message.contains("adk-rust vision models accept sam3-local"));
    }

    #[test]
    fn unreleased_images_are_forbidden() {
        let err: AdkError = VisionError::NotReleased { backend: "gemini".into() }.into();
        assert_eq!(err.category, ErrorCategory::Forbidden);
    }

    #[test]
    fn platform_errors_pass_through_unchanged() {
        let inner = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::ResourceExhausted,
            "budget.exceeded",
            "over budget",
        );
        let err: AdkError = VisionError::Core(inner).into();
        assert_eq!(err.code, "budget.exceeded");
        assert_eq!(err.category, ErrorCategory::ResourceExhausted);
    }
}
