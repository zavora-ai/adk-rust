//! Error types for the evaluation framework

use thiserror::Error;

/// Result type alias for evaluation operations
pub type Result<T> = std::result::Result<T, EvalError>;

/// Errors that can occur during evaluation
#[derive(Error, Debug)]
#[non_exhaustive]
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

    /// Structured judge parse error
    #[error("Structured judge parse error: {0}")]
    JudgeParseError(String),

    /// Embedding scorer error
    #[error("Embedding scorer error: {0}")]
    EmbeddingError(String),

    /// Baseline I/O error
    #[error("Baseline I/O error: {0}")]
    BaselineError(String),

    /// Annotation format error
    #[error("Annotation format error: {0}")]
    AnnotationError(String),

    /// Test generation error
    #[error("Test generation error: {0}")]
    GenerationError(String),

    /// Statistics computation error
    #[error("Statistics computation error: {0}")]
    StatisticsError(String),

    /// A budget, a spend cap or a kill switch refused a model call or run, so nothing was
    /// judged. The evaluation stops rather than counting the refusal as a failed case.
    #[error("evaluation stopped: {0}; raise the evaluation's budget or unfreeze the kill switch")]
    Refused(Box<adk_core::AdkError>),
}

impl EvalError {
    /// Wraps an error from an agent run or a judge call, keeping a budget, spend-cap or
    /// kill-switch refusal ([`is_refusal`]) as [`EvalError::Refused`].
    pub(crate) fn from_model_error(
        error: adk_core::AdkError,
        wrap: impl FnOnce(adk_core::AdkError) -> EvalError,
    ) -> Self {
        if is_refusal(&error) { EvalError::Refused(Box::new(error)) } else { wrap(error) }
    }

    /// Returns `true` for [`EvalError::Refused`].
    pub fn is_refusal(&self) -> bool {
        matches!(self, EvalError::Refused(_))
    }
}

/// Returns `true` when `error` is a refusal by a run budget, a spend ledger or a kill switch
/// rather than a failure of the agent or the judge.
///
/// # Example
///
/// ```
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent, GOVERNANCE_FROZEN_CODE};
///
/// let frozen = AdkError::new(
///     ErrorComponent::Server,
///     ErrorCategory::Forbidden,
///     GOVERNANCE_FROZEN_CODE,
///     "frozen",
/// );
/// assert!(adk_eval::error::is_refusal(&frozen));
/// assert!(!adk_eval::error::is_refusal(&AdkError::model("upstream timeout")));
/// ```
pub fn is_refusal(error: &adk_core::AdkError) -> bool {
    error.code == adk_core::GOVERNANCE_FROZEN_CODE
        || error.code == adk_core::GOVERNANCE_STALE_CODE
        || error.code == adk_core::SPEND_LIMIT_EXCEEDED_CODE
        || error.code.starts_with("budget.")
}

impl From<adk_core::AdkError> for EvalError {
    fn from(err: adk_core::AdkError) -> Self {
        EvalError::from_model_error(err, |err| EvalError::AgentError(err.to_string()))
    }
}

impl From<EvalError> for adk_core::AdkError {
    fn from(err: EvalError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        // A refusal keeps the budget, spend or kill-switch error it carries.
        if let EvalError::Refused(error) = err {
            return *error;
        }
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
            EvalError::JudgeParseError(_) => (ErrorCategory::InvalidInput, "eval.judge_parse"),
            EvalError::EmbeddingError(_) => (ErrorCategory::Internal, "eval.embedding"),
            EvalError::BaselineError(_) => (ErrorCategory::Internal, "eval.baseline"),
            EvalError::AnnotationError(_) => (ErrorCategory::InvalidInput, "eval.annotation"),
            EvalError::GenerationError(_) => (ErrorCategory::Internal, "eval.generation"),
            EvalError::StatisticsError(_) => (ErrorCategory::Internal, "eval.statistics"),
            // Returned as is above.
            EvalError::Refused(_) => (ErrorCategory::ResourceExhausted, "eval.refused"),
        };
        adk_core::AdkError::new(ErrorComponent::Eval, category, code, err.to_string())
            .with_source(err)
    }
}
