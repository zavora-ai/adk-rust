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

impl From<EvalError> for adk_core::AdkError {
    fn from(err: EvalError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            EvalError::LoadError(_) => (ErrorCategory::NotFound, "eval.load"),
            EvalError::ParseError(_) => (ErrorCategory::InvalidInput, "eval.parse"),
            EvalError::ExecutionError(_) => (ErrorCategory::Internal, "eval.execution"),
            EvalError::AgentError(_) => (ErrorCategory::Internal, "eval.agent"),
            EvalError::ConfigError(_) => (ErrorCategory::InvalidInput, "eval.config"),
            EvalError::IoError(_) => (ErrorCategory::Internal, "eval.io"),
            EvalError::JsonError(_) => (ErrorCategory::Internal, "eval.json"),
            EvalError::ScoringError(_) => (ErrorCategory::Internal, "eval.scoring"),
            EvalError::JudgeError(_) => (ErrorCategory::Internal, "eval.judge"),
        };
        adk_core::AdkError::new(ErrorComponent::Eval, category, code, err.to_string())
            .with_source(err)
    }
}
