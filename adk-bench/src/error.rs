//! Error types for the `adk-bench` benchmarking framework.

use thiserror::Error;

/// Errors that can occur during benchmark execution.
#[derive(Debug, Error)]
pub enum BenchError {
    /// Workload JSON failed schema validation.
    #[error("Workload validation failed for field '{field}': {reason}")]
    WorkloadValidation {
        /// The field that failed validation.
        field: String,
        /// Description of the validation failure.
        reason: String,
    },

    /// Workload file was not found at the specified path.
    #[error("Workload file not found: {path}")]
    WorkloadNotFound {
        /// The path that was attempted.
        path: String,
    },

    /// An external framework benchmark subprocess failed.
    #[error("External runner '{framework}' failed: {reason}")]
    ExternalRunner {
        /// The framework that failed.
        framework: String,
        /// Description of the failure.
        reason: String,
    },

    /// An external framework benchmark subprocess exceeded its timeout.
    #[error("External runner '{framework}' timed out after {timeout_secs}s")]
    ExternalTimeout {
        /// The framework that timed out.
        framework: String,
        /// The timeout duration in seconds.
        timeout_secs: u64,
    },

    /// An LLM API call failed during benchmark execution.
    #[error("LLM call failed: {0}")]
    Llm(String),

    /// A baseline persistence operation failed.
    #[error("Baseline operation failed: {0}")]
    Baseline(String),

    /// Serialization or deserialization of benchmark data failed.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Memory sampling is unavailable on the current platform.
    #[error("Memory sampling unavailable on this platform: {0}")]
    MemoryUnavailable(String),

    /// An I/O error occurred.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// A specialized Result type for benchmark operations.
pub type Result<T> = std::result::Result<T, BenchError>;
