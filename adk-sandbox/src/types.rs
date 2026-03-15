//! Core types for sandbox execution: [`Language`], [`ExecRequest`], and [`ExecResult`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// Supported execution languages.
///
/// Each variant maps to a specific interpreter or compiler used by the backend.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::Language;
///
/// let lang = Language::Rust;
/// assert_eq!(lang.to_string(), "rust");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Rust source compiled with `rustc`.
    Rust,
    /// Python source executed with `python3`.
    Python,
    /// JavaScript source executed with `node`.
    JavaScript,
    /// TypeScript source (requires a compatible runtime).
    TypeScript,
    /// Pre-compiled WebAssembly module bytes.
    Wasm,
    /// Raw shell command executed via `sh -c`.
    Command,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::JavaScript => write!(f, "javascript"),
            Language::TypeScript => write!(f, "typescript"),
            Language::Wasm => write!(f, "wasm"),
            Language::Command => write!(f, "command"),
        }
    }
}

/// A request to execute code in a sandbox.
///
/// `ExecRequest` intentionally has no `Default` implementation — callers must
/// explicitly set the `timeout` to avoid unbounded execution.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::{ExecRequest, Language};
/// use std::time::Duration;
/// use std::collections::HashMap;
///
/// let request = ExecRequest {
///     language: Language::Python,
///     code: "print('hello')".to_string(),
///     stdin: None,
///     timeout: Duration::from_secs(30),
///     memory_limit_mb: None,
///     env: HashMap::new(),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct ExecRequest {
    /// The language of the code to execute.
    pub language: Language,
    /// The source code or command to execute.
    pub code: String,
    /// Optional standard input to feed to the process.
    pub stdin: Option<String>,
    /// Maximum wall-clock time allowed for execution. No default — must be set explicitly.
    pub timeout: Duration,
    /// Optional memory limit in megabytes. Only enforced by `WasmBackend`.
    pub memory_limit_mb: Option<u32>,
    /// Environment variables passed to the child process. The backend clears
    /// the inherited environment and sets only these variables.
    pub env: HashMap<String, String>,
}

/// The result of a sandbox execution.
///
/// Non-zero `exit_code` is a valid result, not an error. The caller decides
/// how to interpret the exit code.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::ExecResult;
/// use std::time::Duration;
///
/// let result = ExecResult {
///     stdout: "hello\n".to_string(),
///     stderr: String::new(),
///     exit_code: 0,
///     duration: Duration::from_millis(42),
/// };
/// assert_eq!(result.exit_code, 0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    /// Captured standard output (UTF-8, truncated to 1 MB by backends).
    pub stdout: String,
    /// Captured standard error (UTF-8, truncated to 1 MB by backends).
    pub stderr: String,
    /// Process exit code. 0 typically means success.
    pub exit_code: i32,
    /// Wall-clock duration of the execution.
    pub duration: Duration,
}
