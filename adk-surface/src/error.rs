//! Errors of the engine and its sources (spec 3.3 §6).

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

use crate::rules::RuleViolation;

/// The engine's result.
pub type Result<T> = std::result::Result<T, SurfaceError>;

/// Why the engine refused or failed a read or a command.
///
/// Converts into [`AdkError`] with a `surface.*` code; the web router answers with its problem
/// document and HTTP status.
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// Nothing of that name belongs to the caller. Also the answer for something that exists
    /// but belongs to someone else, so a surface learns nothing about other people's sessions
    /// (spec 3.3 §11 S-I8).
    #[error("{what} was not found among your sessions and decisions")]
    NotFound {
        /// What was looked for.
        what: String,
    },
    /// The answer breaks a rule of spec 3.3 §7.2 or §7.4.
    #[error(transparent)]
    Rule(#[from] RuleViolation),
    /// The caller may not do this from here (spec 3.3 §7.4, §11 S-I10).
    #[error("{0}")]
    NotHere(String),
    /// Stop all or Resume all from a caller outside the group the engine keeps them for
    /// (spec 3.3 §11 S-I10).
    #[error(
        "stopping and resuming every agent needs the `{group}` group: they freeze and unfreeze \
         every run this server makes"
    )]
    OperatorOnly {
        /// The group they need.
        group: String,
    },
    /// The source cannot do this.
    #[error(
        "{what} is not available from this source; run the surface on a kernel or harness that provides it"
    )]
    Unsupported {
        /// What was asked.
        what: String,
    },
    /// The request is malformed.
    #[error("{0}")]
    Invalid(String),
    /// The source failed.
    #[error(transparent)]
    Source(#[from] AdkError),
}

impl SurfaceError {
    /// A [`NotFound`](Self::NotFound) for `what`.
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound { what: what.into() }
    }

    /// An [`Unsupported`](Self::Unsupported) for `what`.
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported { what: what.into() }
    }

    /// The error code: `surface.not_found`, a rule's code, `surface.not_here`,
    /// `surface.operator_only`, `surface.unsupported`, `surface.invalid`, or the source's own
    /// code.
    pub fn code(&self) -> String {
        match self {
            Self::NotFound { .. } => "surface.not_found".to_string(),
            Self::Rule(rule) => rule.code().to_string(),
            Self::NotHere(_) => "surface.not_here".to_string(),
            Self::OperatorOnly { .. } => "surface.operator_only".to_string(),
            Self::Unsupported { .. } => "surface.unsupported".to_string(),
            Self::Invalid(_) => "surface.invalid".to_string(),
            Self::Source(error) => error.code.to_string(),
        }
    }
}

impl From<adk_kernel::KernelError> for SurfaceError {
    fn from(error: adk_kernel::KernelError) -> Self {
        Self::Source(AdkError::from(error))
    }
}

impl From<SurfaceError> for AdkError {
    fn from(error: SurfaceError) -> Self {
        let error = match error {
            SurfaceError::Source(inner) => return inner,
            other => other,
        };
        let (category, code) = match &error {
            SurfaceError::NotFound { .. } => (ErrorCategory::NotFound, "surface.not_found"),
            SurfaceError::Rule(rule) => (ErrorCategory::Forbidden, rule.code()),
            SurfaceError::NotHere(_) => (ErrorCategory::Forbidden, "surface.not_here"),
            SurfaceError::OperatorOnly { .. } => {
                (ErrorCategory::Forbidden, "surface.operator_only")
            }
            SurfaceError::Unsupported { .. } => (ErrorCategory::Unsupported, "surface.unsupported"),
            SurfaceError::Invalid(_) => (ErrorCategory::InvalidInput, "surface.invalid"),
            SurfaceError::Source(_) => (ErrorCategory::Internal, "surface.source"),
        };
        AdkError::new(ErrorComponent::Server, category, code, error.to_string())
    }
}
