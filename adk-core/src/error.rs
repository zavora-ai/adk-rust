/// Unified error type for all ADK operations.
///
/// Each variant corresponds to a layer in the framework. Prefer the most
/// specific variant (e.g. [`Tool`](Self::Tool) over [`Agent`](Self::Agent))
/// so callers can match on the source of failure.
#[derive(Debug, thiserror::Error)]
pub enum AdkError {
    /// Error originating from agent execution or orchestration.
    #[error("Agent error: {0}")]
    Agent(String),

    /// Error from an LLM provider (network, auth, rate limit, bad response).
    #[error("Model error: {0}")]
    Model(String),

    /// Error from tool execution or schema validation.
    #[error("Tool error: {0}")]
    Tool(String),

    /// Error from session creation, retrieval, or state persistence.
    #[error("Session error: {0}")]
    Session(String),

    /// Error from artifact storage or retrieval.
    #[error("Artifact error: {0}")]
    Artifact(String),

    /// Error from the long-term memory / RAG subsystem.
    #[error("Memory error: {0}")]
    Memory(String),

    /// Invalid or missing configuration (API keys, model names, etc.).
    #[error("Configuration error: {0}")]
    Config(String),

    /// Filesystem or network I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization error.
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Convenience alias used throughout ADK crates.
pub type Result<T> = std::result::Result<T, AdkError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = AdkError::Agent("test error".to_string());
        assert_eq!(err.to_string(), "Agent error: test error");
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let adk_err: AdkError = io_err.into();
        assert!(matches!(adk_err, AdkError::Io(_)));
    }

    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn test_result_type() {
        let ok_result: Result<i32> = Ok(42);
        assert_eq!(ok_result.unwrap(), 42);

        let err_result: Result<i32> = Err(AdkError::Config("invalid".to_string()));
        assert!(err_result.is_err());
    }
}
