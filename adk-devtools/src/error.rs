//! Error type for developer-tool operations.

/// Errors produced by the developer tools.
#[derive(Debug, thiserror::Error)]
pub enum DevToolError {
    /// The requested path resolves outside the workspace root.
    #[error("path '{0}' escapes the workspace root")]
    PathEscape(String),

    /// A mutating operation was attempted on a read-only workspace.
    #[error("workspace is read-only; '{0}' is not permitted")]
    ReadOnly(&'static str),

    /// `bash` execution is disabled for this workspace.
    #[error("bash execution is disabled for this workspace")]
    BashDisabled,

    /// `edit_file` was called on a file that has not been read in this session.
    #[error("file '{0}' must be read with read_file before it can be edited")]
    NotRead(String),

    /// The file changed since this session and agent read it, or since the decision that
    /// approved the write was shown: the write did not happen (spec 3.1 §7.26, I66).
    ///
    /// Maps to an [`AdkError`](adk_core::AdkError) with category `Conflict` and code
    /// `devtools.stale`, whose message the model sees.
    #[error("'{path}' changed since it was read: {detail}. Read it again, then make the change")]
    Stale {
        /// The file path.
        path: String,
        /// What differs.
        detail: String,
    },

    /// `old_string` was not found in the target file.
    #[error("no occurrence of the target string was found in '{0}'")]
    NoMatch(String),

    /// `old_string` matched more than once and `replace_all` was not set.
    #[error(
        "the target string occurs {count} times in '{path}'; pass replace_all=true \
         or provide a more specific string"
    )]
    Ambiguous {
        /// The file path.
        path: String,
        /// Number of occurrences found.
        count: usize,
    },

    /// The path matches one of the workspace's exclusion patterns, which by default
    /// name secret files such as `.env` and private keys.
    ///
    /// Maps to an [`AdkError`](adk_core::AdkError) with category `Forbidden` and code
    /// `devtools.path_excluded`, so a caller can ask for approval and read the file
    /// through a workspace that does not exclude it. Implements spec 3.1 §10 D5.
    #[error(
        "'{0}' matches an excluded pattern (secret files such as .env and private keys are \
         not read by default); ask the operator to approve reading it"
    )]
    Excluded(String),

    /// A bash command exceeded its timeout.
    #[error("command timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Any other error (e.g. invalid arguments, bad regex).
    #[error("{0}")]
    Other(String),
}

/// The category, code and whether the model sees the message, for each error the model can
/// act on (spec 3.1 §7.33 fix 6, §10 D9): a rejection's message reaches the model, so it
/// learns to read before editing or to give a more specific string; an I/O failure other
/// than a missing file stays internal and hidden.
impl From<DevToolError> for adk_core::AdkError {
    fn from(e: DevToolError) -> Self {
        use adk_core::ErrorCategory::{
            Conflict, Forbidden, Internal, InvalidInput, NotFound, Timeout,
        };
        let (category, code) = match &e {
            DevToolError::Excluded(_) => (Forbidden, "devtools.path_excluded"),
            DevToolError::NotRead(_) => (Conflict, "devtools.not_read"),
            DevToolError::Stale { .. } => (Conflict, "devtools.stale"),
            DevToolError::NoMatch(_) => (InvalidInput, "devtools.no_match"),
            DevToolError::Ambiguous { .. } => (InvalidInput, "devtools.ambiguous"),
            DevToolError::PathEscape(_) => (Forbidden, "devtools.path_escape"),
            DevToolError::ReadOnly(_) => (Forbidden, "devtools.read_only"),
            DevToolError::BashDisabled => (Forbidden, "devtools.bash_disabled"),
            DevToolError::Timeout(_) => (Timeout, "devtools.timeout"),
            DevToolError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                (NotFound, "devtools.not_found")
            }
            DevToolError::Io(_) => (Internal, "devtools.io"),
            DevToolError::Other(_) => (InvalidInput, "devtools.invalid_argument"),
        };
        adk_core::AdkError::new(adk_core::ErrorComponent::Tool, category, code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::DevToolError;
    use adk_core::{AdkError, ErrorCategory, tool_error_response};

    fn mapped(error: DevToolError) -> (ErrorCategory, String, String) {
        let error: AdkError = error.into();
        let seen = tool_error_response("edit_file", &error, false)["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (error.category, error.code.to_string(), seen)
    }

    #[test]
    fn each_error_the_model_can_act_on_reaches_it_with_its_code() {
        let cases = [
            (
                DevToolError::NotRead("src/lib.rs".into()),
                ErrorCategory::Conflict,
                "devtools.not_read",
            ),
            (
                DevToolError::Stale {
                    path: "src/lib.rs".into(),
                    detail: "it no longer exists".into(),
                },
                ErrorCategory::Conflict,
                "devtools.stale",
            ),
            (
                DevToolError::NoMatch("src/lib.rs".into()),
                ErrorCategory::InvalidInput,
                "devtools.no_match",
            ),
            (
                DevToolError::Ambiguous { path: "src/lib.rs".into(), count: 2 },
                ErrorCategory::InvalidInput,
                "devtools.ambiguous",
            ),
            (
                DevToolError::PathEscape("../x".into()),
                ErrorCategory::Forbidden,
                "devtools.path_escape",
            ),
            (DevToolError::ReadOnly("write_file"), ErrorCategory::Forbidden, "devtools.read_only"),
            (DevToolError::BashDisabled, ErrorCategory::Forbidden, "devtools.bash_disabled"),
            (
                DevToolError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")),
                ErrorCategory::NotFound,
                "devtools.not_found",
            ),
            (
                DevToolError::Other("bad pattern".into()),
                ErrorCategory::InvalidInput,
                "devtools.invalid_argument",
            ),
            (
                DevToolError::Excluded(".env".into()),
                ErrorCategory::Forbidden,
                "devtools.path_excluded",
            ),
        ];
        for (error, category, code) in cases {
            let message = error.to_string();
            let (got_category, got_code, seen) = mapped(error);
            assert_eq!((got_category, got_code.as_str()), (category, code));
            assert!(seen.contains(&message), "the model sees why: {seen}");
            assert!(seen.contains(code), "{seen}");
        }
    }

    #[test]
    fn the_not_read_message_tells_the_model_what_to_do() {
        let (_, _, seen) = mapped(DevToolError::NotRead("src/lib.rs".into()));
        assert_eq!(
            seen,
            "tool 'edit_file' failed: tool.conflict (devtools.not_read): file 'src/lib.rs' must be \
             read with read_file before it can be edited"
        );
    }

    #[test]
    fn other_io_failures_and_timeouts_stay_hidden() {
        let (category, code, seen) =
            mapped(DevToolError::Io(std::io::Error::other("disk at /secret")));
        assert_eq!((category, code.as_str()), (ErrorCategory::Internal, "devtools.io"));
        assert!(!seen.contains("/secret"), "{seen}");
        let (category, _, seen) = mapped(DevToolError::Timeout(std::time::Duration::from_secs(5)));
        assert_eq!(category, ErrorCategory::Timeout);
        assert!(seen.contains("devtools.timeout"), "{seen}");
    }
}
