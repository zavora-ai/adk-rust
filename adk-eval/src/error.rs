//! Error types for the evaluation framework

use thiserror::Error;

/// Result type alias for evaluation operations
pub type Result<T> = std::result::Result<T, EvalError>;

/// Errors that can occur during evaluation
#[derive(Error, Debug)]
pub enum EvalError {
    /// Failed to load test file
    #[error("Failed to load test file: {0}")]
    LoadError(String),

    /// Failed to parse test file
    #[error("Failed to parse test file: {0}")]
    ParseError(String),

    /// Test case execution failed
    #[error("Test case execution failed: {0}")]
    ExecutionError(String),

    /// Agent error during evaluation
    #[error("Agent error: {0}")]
    AgentError(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON serialization error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Scoring error
    #[error("Scoring error: {0}")]
    ScoringError(String),

    /// LLM judge error
    #[error("LLM judge error: {0}")]
    JudgeError(String),
}

impl From<adk_core::AdkError> for EvalError {
    fn from(err: adk_core::AdkError) -> Self {
        EvalError::AgentError(err.to_string())
    }
}
