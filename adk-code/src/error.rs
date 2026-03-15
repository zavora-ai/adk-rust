//! Error types for code execution.

use thiserror::Error;

use crate::diagnostics::RustDiagnostic;

/// Errors that can occur during code execution.
///
/// Each variant provides actionable context about what went wrong and why.
///
/// # Example
///
/// ```rust
/// use adk_code::ExecutionError;
///
/// let err = ExecutionError::CompileFailed("missing semicolon on line 3".to_string());
/// assert!(err.to_string().contains("compilation failed"));
/// ```
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    /// The backend cannot enforce a requested sandbox policy control.
    #[error("unsupported policy: {0}")]
    UnsupportedPolicy(String),

    /// The backend does not support the requested language.
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),

    /// Rust or other compiled language failed to build.
    #[error("compilation failed: {0}")]
    CompileFailed(String),

    /// Execution exceeded the configured timeout.
    #[error("execution timeout after {0}ms")]
    Timeout(u64),

    /// Runtime execution failed.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// Execution was rejected before running (e.g., policy or scope check).
    #[error("rejected: {0}")]
    Rejected(String),

    /// The execution request is malformed or missing required fields.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Internal error (e.g., thread panic, unexpected runtime failure).
    #[error("internal error: {0}")]
    InternalError(String),
}

/// Errors from the language-aware code pipeline ([`RustExecutor`]).
///
/// Unlike [`ExecutionError`] (which covers the legacy executor), `CodeError`
/// carries structured diagnostics for compile failures and distinguishes
/// missing dependencies from sandbox-level failures.
///
/// # Example
///
/// ```rust
/// use adk_code::CodeError;
///
/// let err = CodeError::InvalidCode("missing `fn run()` entry point".to_string());
/// assert!(err.to_string().contains("invalid code"));
/// ```
#[derive(Debug, Clone, Error)]
pub enum CodeError {
    /// Compilation produced one or more error-level diagnostics.
    #[error("compile error: {stderr}")]
    CompileError {
        /// Structured diagnostics parsed from `--error-format=json`.
        diagnostics: Vec<RustDiagnostic>,
        /// Raw stderr output from the compiler.
        stderr: String,
    },

    /// A required dependency could not be located on disk.
    #[error("dependency not found: {name} (searched: {searched:?})")]
    DependencyNotFound {
        /// Crate name that was not found (e.g., `"serde_json"`).
        name: String,
        /// Paths that were searched before giving up.
        searched: Vec<String>,
    },

    /// The underlying sandbox backend returned an error.
    #[error("sandbox error: {0}")]
    Sandbox(#[from] adk_sandbox::SandboxError),

    /// The source code is invalid before compilation is attempted.
    #[error("invalid code: {0}")]
    InvalidCode(String),
}
