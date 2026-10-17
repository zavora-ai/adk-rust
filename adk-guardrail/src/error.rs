use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Severity;

/// The [`AdkError::code`](adk_core::AdkError) of a user message input guardrails block.
pub const INPUT_BLOCKED_CODE: &str = "guardrail.input_blocked";

/// The [`AdkError::code`](adk_core::AdkError) of a model turn or tool result output
/// guardrails block.
pub const OUTPUT_BLOCKED_CODE: &str = "guardrail.output_blocked";

/// The event metadata key under which an agent records the low-severity failures of the
/// output guardrails that screened the event's content, as a JSON array of
/// [`GuardrailFailure`]s.
pub const GUARDRAIL_WARNINGS_KEY: &str = "adk.guardrail.warnings";

/// What a guardrail set was screening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailPhase {
    /// A user message, before the agent or the session sees it.
    Input,
    /// What the agent produces: model text and results that end the turn.
    Output,
}

impl fmt::Display for GuardrailPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Input => "input",
            Self::Output => "output",
        })
    }
}

/// One guardrail's failing verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailFailure {
    /// Name of the guardrail that failed the content.
    pub guardrail: String,
    /// Why it failed the content.
    pub reason: String,
    /// How serious the failure is. [`Severity::Low`] does not block.
    pub severity: Severity,
}

/// Errors produced by guardrail validation.
#[derive(Debug, thiserror::Error)]
pub enum GuardrailError {
    /// A single guardrail check failed.
    #[error("Guardrail '{name}' failed: {reason}")]
    ValidationFailed { name: String, reason: String, severity: Severity },

    /// Multiple guardrails failed in a single execution.
    #[error("Multiple guardrails failed: {0:?}")]
    MultipleFailures(Vec<GuardrailError>),

    /// A guardrail set blocked content: at least one failure is above [`Severity::Low`].
    ///
    /// Converts to an [`AdkError`](adk_core::AdkError) with category `InvalidInput`, code
    /// [`INPUT_BLOCKED_CODE`] or [`OUTPUT_BLOCKED_CODE`], and details metadata naming the
    /// blocking `guardrail`, its `severity`, the `phase`, and every `failures` entry.
    #[error("{phase} guardrails blocked content: {}", describe(.failures))]
    Blocked {
        /// What was being screened.
        phase: GuardrailPhase,
        /// Every failure the set reported, in the order the guardrails ran.
        failures: Vec<GuardrailFailure>,
    },

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

fn describe(failures: &[GuardrailFailure]) -> String {
    failures
        .iter()
        .map(|failure| {
            format!("{} ({:?}): {}", failure.guardrail, failure.severity, failure.reason)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Convenience alias for guardrail operations.
pub type Result<T> = std::result::Result<T, GuardrailError>;

impl From<GuardrailError> for adk_core::AdkError {
    fn from(err: GuardrailError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent, ErrorDetails};
        let (category, code) = match &err {
            GuardrailError::ValidationFailed { .. } => {
                (ErrorCategory::InvalidInput, "guardrail.validation_failed")
            }
            GuardrailError::MultipleFailures(_) => {
                (ErrorCategory::InvalidInput, "guardrail.multiple_failures")
            }
            GuardrailError::Blocked { phase: GuardrailPhase::Input, .. } => {
                (ErrorCategory::InvalidInput, INPUT_BLOCKED_CODE)
            }
            GuardrailError::Blocked { phase: GuardrailPhase::Output, .. } => {
                (ErrorCategory::InvalidInput, OUTPUT_BLOCKED_CODE)
            }
            GuardrailError::Schema(_) => (ErrorCategory::InvalidInput, "guardrail.schema"),
            GuardrailError::Regex(_) => (ErrorCategory::InvalidInput, "guardrail.regex"),
            GuardrailError::Internal(_) => (ErrorCategory::Internal, "guardrail.internal"),
        };
        let mut details = ErrorDetails::default();
        match &err {
            GuardrailError::ValidationFailed { name, severity, .. } => {
                details.metadata.insert("guardrail".to_string(), json!(name));
                details.metadata.insert("severity".to_string(), json!(severity));
            }
            GuardrailError::Blocked { phase, failures } => {
                // The blocking guardrail is the first whose failure is above `Low`.
                let blocking = failures
                    .iter()
                    .find(|failure| failure.severity != Severity::Low)
                    .or_else(|| failures.first());
                if let Some(blocking) = blocking {
                    details.metadata.insert("guardrail".to_string(), json!(blocking.guardrail));
                    details.metadata.insert("severity".to_string(), json!(blocking.severity));
                }
                details.metadata.insert("phase".to_string(), json!(phase));
                details.metadata.insert(
                    "failures".to_string(),
                    serde_json::to_value(failures).unwrap_or(Value::Null),
                );
            }
            GuardrailError::MultipleFailures(_)
            | GuardrailError::Schema(_)
            | GuardrailError::Regex(_)
            | GuardrailError::Internal(_) => {}
        }
        adk_core::AdkError::new(ErrorComponent::Guardrail, category, code, err.to_string())
            .with_details(details)
            .with_source(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{AdkError, ErrorCategory, ErrorComponent};

    #[test]
    fn a_block_converts_to_a_structured_invalid_input_error() {
        let err: AdkError = GuardrailError::Blocked {
            phase: GuardrailPhase::Output,
            failures: vec![
                GuardrailFailure {
                    guardrail: "tone".into(),
                    reason: "informal".into(),
                    severity: Severity::Low,
                },
                GuardrailFailure {
                    guardrail: "blocked_keywords".into(),
                    reason: "matched 1 pattern".into(),
                    severity: Severity::High,
                },
            ],
        }
        .into();
        assert_eq!(err.component, ErrorComponent::Guardrail);
        assert_eq!(err.category, ErrorCategory::InvalidInput);
        assert_eq!(err.code, OUTPUT_BLOCKED_CODE);
        assert_eq!(err.http_status_code(), 400);
        let metadata = &err.details.metadata;
        assert_eq!(metadata["guardrail"], json!("blocked_keywords"));
        assert_eq!(metadata["severity"], json!("High"));
        assert_eq!(metadata["phase"], json!("output"));
        assert_eq!(metadata["failures"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            err.message,
            "output guardrails blocked content: tone (Low): informal; blocked_keywords (High): \
             matched 1 pattern"
        );
    }

    #[test]
    fn an_input_block_has_its_own_code() {
        let err: AdkError = GuardrailError::Blocked {
            phase: GuardrailPhase::Input,
            failures: vec![GuardrailFailure {
                guardrail: "harmful_content".into(),
                reason: "matched".into(),
                severity: Severity::Critical,
            }],
        }
        .into();
        assert_eq!(err.code, INPUT_BLOCKED_CODE);
        assert_eq!(err.details.metadata["severity"], json!("Critical"));
    }
}
