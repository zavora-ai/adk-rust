//! Errors of the site pipeline.

use std::path::PathBuf;

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

use crate::scan::ServerCodeFinding;

/// The result type of `adk-site`.
pub type Result<T> = std::result::Result<T, SiteError>;

/// An error from the site pipeline (spec 3.5 §7.8).
///
/// Converts into [`AdkError`] with component `Deploy` and a `site.*` code.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SiteError {
    /// A file or directory could not be read or written.
    #[error("i/o error at '{}': {source}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The site configuration is invalid.
    #[error("site configuration is invalid: {0}. Fix site.toml and load the project again.")]
    InvalidConfig(String),

    /// A content item is invalid.
    #[error(
        "content item '{path}' is invalid: {message}. Fix the item and load the project again."
    )]
    InvalidContent {
        /// The item's file, or its id when it has no file.
        path: String,
        /// What is wrong.
        message: String,
    },

    /// A theme template failed to load or render.
    #[error("template '{template}' failed: {message}. Check the theme's templates.")]
    Template {
        /// The template name.
        template: String,
        /// The template engine's message.
        message: String,
    },

    /// The release holds server-executable files or server-side include directives
    /// (spec 3.5 A-I1). No flag overrides this refusal.
    #[error(
        "release refused: {} server-executable file(s) or server-side include directive(s) (A-I1), \
         first '{}'. Remove them from the output and build again.",
        findings.len(),
        findings.first().map_or("", |f| f.path.as_str())
    )]
    ServerCode {
        /// Every finding.
        findings: Vec<ServerCodeFinding>,
    },

    /// A release, rollback target or stored file failed a digest check.
    #[error("digest mismatch for '{path}': expected {expected}, found {actual}")]
    DigestMismatch {
        /// The file.
        path: String,
        /// The digest the manifest names.
        expected: String,
        /// The digest of what is there.
        actual: String,
    },

    /// A release a host or log was asked for does not exist.
    #[error("release {seq} not found: {message}")]
    ReleaseNotFound {
        /// The release sequence number.
        seq: u64,
        /// Where it was looked for.
        message: String,
    },

    /// A release does not follow the one before it.
    #[error("release sequence error: {0}")]
    Sequence(String),

    /// Signing a release failed.
    #[error("release signing failed: {0}")]
    Signing(String),

    /// The site log failed.
    #[error("site log error: {0}")]
    Log(String),

    /// A host failed.
    #[error("host '{host}' failed: {message}")]
    Host {
        /// The host's name.
        host: String,
        /// What failed.
        message: String,
    },

    /// An uploaded media file was refused.
    #[error("media '{name}' refused: {reason}")]
    MediaRefused {
        /// The upload's name.
        name: String,
        /// Why it was refused.
        reason: String,
    },

    /// The launch gate failed.
    #[error("launch gate failed: {0}")]
    GateFailed(String),

    /// A publishing step was asked for out of order or for a change that is not ready.
    #[error("publishing pipeline: {0}")]
    Pipeline(String),

    /// A content item was not found.
    #[error("content item '{0}' not found")]
    NotFound(String),

    /// An external generator could not run or failed (spec 3.5 §7.8 "Generator").
    #[error("generator '{generator}' failed: {message}")]
    Generator {
        /// The generator's name.
        generator: String,
        /// What failed.
        message: String,
    },

    /// Serialising or parsing a document failed.
    #[error("serialisation failed for {what}: {message}")]
    Serde {
        /// What was being serialised or parsed.
        what: String,
        /// The serialiser's message.
        message: String,
    },
}

impl SiteError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    pub(crate) fn serde(what: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self::Serde { what: what.into(), message: message.to_string() }
    }

    pub(crate) fn host(host: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Host { host: host.into(), message: message.into() }
    }

    /// The stable error code, `site.<kind>`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "site.io",
            Self::InvalidConfig(_) => "site.invalid_config",
            Self::InvalidContent { .. } => "site.invalid_content",
            Self::Template { .. } => "site.template",
            Self::ServerCode { .. } => "site.release.server_code",
            Self::DigestMismatch { .. } => "site.digest_mismatch",
            Self::ReleaseNotFound { .. } => "site.release.not_found",
            Self::Sequence(_) => "site.release.sequence",
            Self::Signing(_) => "site.release.signing_failed",
            Self::Log(_) => "site.log",
            Self::Host { .. } => "site.host",
            Self::MediaRefused { .. } => "site.media.refused",
            Self::GateFailed(_) => "site.gate.failed",
            Self::Pipeline(_) => "site.pipeline",
            Self::NotFound(_) => "site.not_found",
            Self::Generator { .. } => "site.generator",
            Self::Serde { .. } => "site.serde",
        }
    }

    fn category(&self) -> ErrorCategory {
        match self {
            Self::Io { .. } | Self::Host { .. } | Self::Signing(_) | Self::Generator { .. } => {
                ErrorCategory::Unavailable
            }
            Self::InvalidConfig(_)
            | Self::InvalidContent { .. }
            | Self::Template { .. }
            | Self::MediaRefused { .. }
            | Self::Serde { .. } => ErrorCategory::InvalidInput,
            Self::ServerCode { .. } | Self::GateFailed(_) => ErrorCategory::Forbidden,
            Self::DigestMismatch { .. } | Self::Log(_) => ErrorCategory::Internal,
            Self::ReleaseNotFound { .. } | Self::NotFound(_) => ErrorCategory::NotFound,
            Self::Sequence(_) | Self::Pipeline(_) => ErrorCategory::Conflict,
        }
    }
}

impl From<SiteError> for AdkError {
    fn from(error: SiteError) -> Self {
        AdkError::new(ErrorComponent::Deploy, error.category(), error.code(), error.to_string())
    }
}

impl From<AdkError> for SiteError {
    fn from(error: AdkError) -> Self {
        Self::Log(error.to_string())
    }
}
