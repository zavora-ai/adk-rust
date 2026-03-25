use crate::Severity;

/// Errors produced by guardrail validation.
#[derive(Debug, thiserror::Error)]
pub enum GuardrailError {
    /// A single guardrail check failed.
    #[error("Guardrail '{name}' failed: {reason}")]
    ValidationFailed { name: String, reason: String, severity: Severity },

    /// Multiple guardrails failed in a single execution.
    #[error("Multiple guardrails failed: {0:?}")]
    MultipleFailures(Vec<GuardrailError>),

    /// JSON schema validation error.
    #[error("Schema validation error: {0}")]
    Schema(String),

    /// Invalid regex pattern in a guardrail configuration.
    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    /// Unexpected internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Convenience alias for guardrail operations.
pub type Result<T> = std::result::Result<T, GuardrailError>;

impl From<GuardrailError> for adk_core::AdkError {
    fn from(err: GuardrailError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            GuardrailError::ValidationFailed { .. } => {
                (ErrorCategory::InvalidInput, "guardrail.validation_failed")
            }
            GuardrailError::MultipleFailures(_) => {
                (ErrorCategory::InvalidInput, "guardrail.multiple_failures")
            }
            GuardrailError::Schema(_) => (ErrorCategory::InvalidInput, "guardrail.schema"),
            GuardrailError::Regex(_) => (ErrorCategory::InvalidInput, "guardrail.regex"),
            GuardrailError::Internal(_) => (ErrorCategory::Internal, "guardrail.internal"),
        };
        adk_core::AdkError::new(ErrorComponent::Guardrail, category, code, err.to_string())
            .with_source(err)
    }
}
