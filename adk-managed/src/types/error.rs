//! Runtime error types for the managed agent runtime.

use thiserror::Error;

/// Runtime errors, one variant per failure a caller handles differently.
///
/// Each variant represents a distinct failure mode with structured context
/// for programmatic handling by the platform layer.
///
/// # Example
///
/// ```
/// use adk_managed::types::RuntimeError;
///
/// let err = RuntimeError::NotFound {
///     session_id: "ses_abc123".to_string(),
/// };
/// assert_eq!(err.to_string(), "session not found: ses_abc123");
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The request is malformed or contains invalid parameters.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// Description of what is invalid.
        message: String,
        /// The specific parameter that is invalid, if applicable.
        param: Option<String>,
    },

    /// The requested session was not found.
    #[error("session not found: {session_id}")]
    NotFound {
        /// The session ID that could not be found.
        session_id: String,
    },

    /// A state conflict occurred (e.g., invalid status transition).
    #[error("conflict: {message}")]
    Conflict {
        /// Description of the conflict.
        message: String,
    },

    /// The underlying LLM provider returned an error.
    #[error("provider error ({provider}): {message}")]
    ProviderError {
        /// The provider that failed (e.g., "gemini", "openai").
        provider: String,
        /// The error message from the provider.
        message: String,
    },

    /// A tool call timed out waiting for a response.
    #[error("tool timeout: {tool_use_id} after {timeout_secs}s")]
    ToolTimeout {
        /// The ID of the tool call that timed out.
        tool_use_id: String,
        /// The timeout duration in seconds.
        timeout_secs: u64,
    },

    /// A checkpoint persistence operation failed.
    #[error("checkpoint failed: {message}")]
    CheckpointFailed {
        /// Description of the checkpoint failure.
        message: String,
    },

    /// A sandbox execution error occurred.
    #[error("sandbox error: {message}")]
    SandboxError {
        /// Description of the sandbox error.
        message: String,
    },

    /// An internal runtime error that should not normally occur.
    #[error("internal error: {message}")]
    Internal {
        /// Description of the internal error.
        message: String,
    },
}

impl RuntimeError {
    /// Creates an `InvalidRequest` error with the given message.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest { message: message.into(), param: None }
    }

    /// Creates an `InvalidRequest` error with a specific parameter name.
    pub fn invalid_param(message: impl Into<String>, param: impl Into<String>) -> Self {
        Self::InvalidRequest { message: message.into(), param: Some(param.into()) }
    }

    /// Creates a `NotFound` error for the given session ID.
    pub fn not_found(session_id: impl Into<String>) -> Self {
        Self::NotFound { session_id: session_id.into() }
    }

    /// Creates a `Conflict` error with the given message.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict { message: message.into() }
    }

    /// Creates a `ProviderError` with provider name and message.
    pub fn provider_error(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ProviderError { provider: provider.into(), message: message.into() }
    }

    /// Creates a `ToolTimeout` error.
    pub fn tool_timeout(tool_use_id: impl Into<String>, timeout_secs: u64) -> Self {
        Self::ToolTimeout { tool_use_id: tool_use_id.into(), timeout_secs }
    }

    /// Creates a `CheckpointFailed` error.
    pub fn checkpoint_failed(message: impl Into<String>) -> Self {
        Self::CheckpointFailed { message: message.into() }
    }

    /// Creates a `SandboxError`.
    pub fn sandbox_error(message: impl Into<String>) -> Self {
        Self::SandboxError { message: message.into() }
    }

    /// Creates an `Internal` error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal { message: message.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_request_display() {
        let err = RuntimeError::invalid_request("missing field 'model'");
        assert_eq!(err.to_string(), "invalid request: missing field 'model'");
    }

    #[test]
    fn test_invalid_param_display() {
        let err = RuntimeError::invalid_param("must be positive", "timeout_ms");
        assert_eq!(err.to_string(), "invalid request: must be positive");
    }

    #[test]
    fn test_not_found_display() {
        let err = RuntimeError::not_found("ses_abc123");
        assert_eq!(err.to_string(), "session not found: ses_abc123");
    }

    #[test]
    fn test_conflict_display() {
        let err = RuntimeError::conflict("cannot transition from Archived to Running");
        assert_eq!(err.to_string(), "conflict: cannot transition from Archived to Running");
    }

    #[test]
    fn test_provider_error_display() {
        let err = RuntimeError::provider_error("openai", "rate limit exceeded");
        assert_eq!(err.to_string(), "provider error (openai): rate limit exceeded");
    }

    #[test]
    fn test_tool_timeout_display() {
        let err = RuntimeError::tool_timeout("tool_use_xyz", 300);
        assert_eq!(err.to_string(), "tool timeout: tool_use_xyz after 300s");
    }

    #[test]
    fn test_checkpoint_failed_display() {
        let err = RuntimeError::checkpoint_failed("database connection lost");
        assert_eq!(err.to_string(), "checkpoint failed: database connection lost");
    }

    #[test]
    fn test_sandbox_error_display() {
        let err = RuntimeError::sandbox_error("container crashed");
        assert_eq!(err.to_string(), "sandbox error: container crashed");
    }

    #[test]
    fn test_internal_error_display() {
        let err = RuntimeError::internal("unexpected state");
        assert_eq!(err.to_string(), "internal error: unexpected state");
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimeError>();
    }

    #[test]
    fn test_error_variants_have_structured_fields() {
        // Verify that structured fields are accessible for programmatic handling
        let err = RuntimeError::ToolTimeout { tool_use_id: "tu_123".to_string(), timeout_secs: 60 };

        if let RuntimeError::ToolTimeout { tool_use_id, timeout_secs } = &err {
            assert_eq!(tool_use_id, "tu_123");
            assert_eq!(*timeout_secs, 60);
        } else {
            panic!("expected ToolTimeout variant");
        }
    }
}
