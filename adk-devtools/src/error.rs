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

impl From<DevToolError> for adk_core::AdkError {
    fn from(e: DevToolError) -> Self {
        match e {
            DevToolError::Excluded(_) => adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::Forbidden,
                "devtools.path_excluded",
                e.to_string(),
            ),
            DevToolError::PathEscape(_)
            | DevToolError::ReadOnly(_)
            | DevToolError::BashDisabled
            | DevToolError::NotRead(_)
            | DevToolError::NoMatch(_)
            | DevToolError::Ambiguous { .. }
            | DevToolError::Timeout(_)
            | DevToolError::Io(_)
            | DevToolError::Other(_) => adk_core::AdkError::tool(e.to_string()),
        }
    }
}
