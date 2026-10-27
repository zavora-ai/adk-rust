//! Why a screen action was refused or failed (spec 3.3 §7.4, §11).

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// Why a screen action was refused or failed (spec 3.3 §2 item 7, §7.4).
///
/// Converts into [`AdkError`] with a `surface.verb.*` code; the web router answers with its
/// problem document and HTTP status. Every message is plain words a surface may show.
///
/// | Variant | Code | Status |
/// |---|---|---|
/// | [`Unknown`](Self::Unknown), [`NotFound`](Self::NotFound) | `surface.verb.unknown` | 404 |
/// | [`Reserved`](Self::Reserved) | `surface.verb.reserved` | 400 |
/// | [`NotHere`](Self::NotHere) | `surface.verb.not_here` | 403 |
/// | [`Invalid`](Self::Invalid) | `surface.verb.invalid` | 400 |
/// | [`Ambiguous`](Self::Ambiguous) | `surface.verb.ambiguous` | 409 |
/// | [`Refused`](Self::Refused) | `surface.verb.refused` | 409 |
/// | [`Source`](Self::Source) | the source's code | the source's status |
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::VerbError;
///
/// let error = VerbError::refused("It moved since you saw this");
/// assert_eq!(error.code(), "surface.verb.refused");
/// assert_eq!(adk_core::AdkError::from(error).http_status_code(), 409);
/// ```
#[derive(Debug, thiserror::Error)]
pub enum VerbError {
    /// No action of that name is offered here.
    #[error("there is no screen action called {verb}; GET /api/surface/verbs lists them")]
    Unknown {
        /// The name asked for.
        verb: String,
    },
    /// Nothing the caller may act on has that id or title. Also the answer for something that
    /// exists but is someone else's or in another area, so a caller learns nothing about it
    /// (spec 3.3 §11 S-I8).
    #[error("nothing in Coming up is called {what}")]
    NotFound {
        /// What was looked for.
        what: String,
    },
    /// The name is one of the decision, agent, governance and resume words no screen action
    /// may take (spec 3.3 §7.4, S-I4, S-I10, S-I11).
    #[error(
        "`{name}` is reserved: a screen action never answers a decision, changes an agent, a \
         rule, a mode or Stop all, or resumes work; choose another name"
    )]
    Reserved {
        /// The name.
        name: String,
    },
    /// The caller may not take this action from here: a chat channel or the tray, an agent
    /// asking for an action only a person takes, or a remote terminal (spec 3.3 §7.2).
    #[error("{0}")]
    NotHere(String),
    /// The request is malformed: a name outside `^[a-z][a-z0-9_]{0,58}$`, an object kind the
    /// action does not take, arguments its schema refuses.
    #[error("{0}")]
    Invalid(String),
    /// More than one thing matches what was said or typed.
    #[error("{} things match “{said}” — use the full name: {}", matches.len(), matches.join(", "))]
    Ambiguous {
        /// What was said or typed.
        said: String,
        /// The titles that match, in Coming up order.
        matches: Vec<String>,
    },
    /// The action cannot be taken now: the item started, moved since it was shown, ends too
    /// soon, or the governed path refused it.
    #[error("{0}")]
    Refused(String),
    /// The kernel or a store failed.
    #[error(transparent)]
    Source(#[from] AdkError),
}

impl VerbError {
    /// A [`Refused`](Self::Refused) saying `why`.
    pub fn refused(why: impl Into<String>) -> Self {
        Self::Refused(why.into())
    }

    /// An [`Invalid`](Self::Invalid) saying `why`.
    pub fn invalid(why: impl Into<String>) -> Self {
        Self::Invalid(why.into())
    }

    /// A [`NotFound`](Self::NotFound) for `what`.
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound { what: what.into() }
    }

    /// The error code: `surface.verb.unknown`, `surface.verb.reserved`,
    /// `surface.verb.not_here`, `surface.verb.invalid`, `surface.verb.ambiguous`,
    /// `surface.verb.refused`, or the source's own code.
    pub fn code(&self) -> String {
        match self {
            Self::Source(error) => error.code.to_string(),
            other => other.parts().1.to_string(),
        }
    }

    fn parts(&self) -> (ErrorCategory, &'static str) {
        match self {
            Self::Unknown { .. } | Self::NotFound { .. } => {
                (ErrorCategory::NotFound, "surface.verb.unknown")
            }
            Self::Reserved { .. } => (ErrorCategory::InvalidInput, "surface.verb.reserved"),
            Self::NotHere(_) => (ErrorCategory::Forbidden, "surface.verb.not_here"),
            Self::Invalid(_) => (ErrorCategory::InvalidInput, "surface.verb.invalid"),
            Self::Ambiguous { .. } => (ErrorCategory::Conflict, "surface.verb.ambiguous"),
            Self::Refused(_) => (ErrorCategory::Conflict, "surface.verb.refused"),
            Self::Source(_) => (ErrorCategory::Internal, "surface.verb.source"),
        }
    }
}

impl From<adk_kernel::KernelError> for VerbError {
    fn from(error: adk_kernel::KernelError) -> Self {
        Self::Source(AdkError::from(error))
    }
}

impl From<VerbError> for AdkError {
    fn from(error: VerbError) -> Self {
        if let VerbError::Source(inner) = error {
            return inner;
        }
        let (category, code) = error.parts();
        AdkError::new(ErrorComponent::Server, category, code, error.to_string())
    }
}

impl From<VerbError> for crate::SurfaceError {
    fn from(error: VerbError) -> Self {
        Self::Source(AdkError::from(error))
    }
}
