//! Error types for the functional API.

use crate::error::GraphError;

/// Errors specific to the functional API.
#[derive(Debug, thiserror::Error)]
pub enum FunctionalError {
    /// Task execution failed after all retry attempts.
    #[error("task '{task}' failed after {attempts} attempts: {message}")]
    TaskFailed {
        /// Name of the failed task.
        task: String,
        /// Number of attempts made.
        attempts: u32,
        /// Failure message.
        message: String,
    },

    /// State schema validation error.
    #[error("state validation failed for field '{field}': expected {expected}, got {actual}")]
    SchemaValidation {
        /// The field that failed validation.
        field: String,
        /// The expected type or value.
        expected: String,
        /// The actual type or value.
        actual: String,
    },

    /// The workflow suspended at an interrupt and needs a resume value.
    ///
    /// `continuation_key` identifies the interrupt site. Supply the value under that key via
    /// `TaskContext::with_resume_values` and re-invoke the entrypoint; the interrupt call then
    /// returns the value instead of this error.
    ///
    /// Previously an interrupt always produced `InterruptTypeMismatch`, so a caller could not
    /// tell "needs input" from "the value you gave me was the wrong type" — and there was no
    /// key to supply the value under.
    #[error("workflow suspended at interrupt '{continuation_key}': {message}")]
    Suspended {
        /// Key identifying this interrupt site within the thread.
        continuation_key: String,
        /// The message passed to `interrupt`.
        message: String,
    },

    /// Interrupt deserialization error (wrong type provided on resume).
    #[error("interrupt resume type mismatch for task '{task}': {message}")]
    InterruptTypeMismatch {
        /// The task that was interrupted.
        task: String,
        /// Description of the type mismatch.
        message: String,
    },

    /// Workflow was cancelled via cancellation token.
    #[error("workflow cancelled")]
    Cancelled,

    /// Checkpoint persistence failure.
    #[error("checkpoint failed for task '{task}': {message}")]
    CheckpointFailed {
        /// The task whose checkpoint failed.
        task: String,
        /// Description of the checkpoint failure.
        message: String,
    },

    /// Background run timeout exceeded.
    #[error("run '{run_id}' timed out after {timeout_secs}s")]
    RunTimeout {
        /// The run identifier.
        run_id: String,
        /// The timeout duration in seconds.
        timeout_secs: u64,
    },

    /// Invalid cron expression.
    #[error("invalid cron expression '{expression}': {reason}")]
    InvalidCronExpression {
        /// The invalid cron expression.
        expression: String,
        /// Reason the expression is invalid.
        reason: String,
    },

    /// Cron job not found.
    #[error("cron job '{job_id}' not found")]
    CronJobNotFound {
        /// The job identifier that was not found.
        job_id: String,
    },

    /// Background run not found.
    #[error("run '{run_id}' not found")]
    RunNotFound {
        /// The run identifier that was not found.
        run_id: String,
    },
}

impl From<FunctionalError> for GraphError {
    fn from(e: FunctionalError) -> Self {
        GraphError::Other(e.to_string())
    }
}
