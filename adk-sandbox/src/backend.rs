//! The [`SandboxBackend`] trait and [`BackendCapabilities`] descriptor.

use async_trait::async_trait;

use crate::error::SandboxError;
use crate::types::{ExecRequest, ExecResult, Language};

/// Async trait for isolated code execution backends.
///
/// Implementations provide a single `execute()` method that runs code in
/// isolation and returns the result. The trait is intentionally minimal —
/// no lifecycle methods (start/stop/restart). `ProcessBackend` is stateless
/// and `WasmBackend` creates a fresh instance per call.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::{SandboxBackend, ExecRequest, ExecResult, SandboxError};
///
/// struct MyBackend;
///
/// #[async_trait::async_trait]
/// impl SandboxBackend for MyBackend {
///     fn name(&self) -> &str { "my-backend" }
///     fn capabilities(&self) -> BackendCapabilities { /* ... */ }
///     async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError> {
///         // Execute code in isolation
///         todo!()
///     }
/// }
/// ```
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Returns the backend name (e.g., `"process"`, `"wasm"`).
    fn name(&self) -> &str;

    /// Returns the capabilities and enforced limits of this backend.
    fn capabilities(&self) -> BackendCapabilities;

    /// Executes code in isolation according to the request parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] if execution cannot complete (timeout,
    /// memory exceeded, invalid request, etc.). A non-zero exit code
    /// is **not** an error — it is returned in [`ExecResult::exit_code`].
    async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError>;
}

/// Describes what a backend supports and enforces.
///
/// Callers can inspect capabilities to choose the right backend or to
/// understand what isolation guarantees are provided.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::{BackendCapabilities, EnforcedLimits, Language};
///
/// let caps = BackendCapabilities {
///     supported_languages: vec![Language::Python, Language::JavaScript],
///     isolation_class: "process".to_string(),
///     enforced_limits: EnforcedLimits {
///         timeout: true,
///         memory: false,
///         network_isolation: false,
///         filesystem_isolation: false,
///         environment_isolation: true,
///     },
/// };
/// assert!(caps.enforced_limits.timeout);
/// ```
#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    /// Languages this backend can execute.
    pub supported_languages: Vec<Language>,
    /// Isolation class identifier (e.g., `"process"`, `"wasm"`, `"container"`).
    pub isolation_class: String,
    /// Which resource limits the backend actually enforces.
    pub enforced_limits: EnforcedLimits,
}

/// Describes which resource limits a backend enforces.
///
/// Backends are honest about what they enforce. For example,
/// `ProcessBackend` enforces timeout and environment isolation
/// but not memory or network isolation.
#[derive(Debug, Clone)]
pub struct EnforcedLimits {
    /// Whether the backend enforces execution timeout.
    pub timeout: bool,
    /// Whether the backend enforces memory limits.
    pub memory: bool,
    /// Whether the backend isolates network access.
    pub network_isolation: bool,
    /// Whether the backend isolates filesystem access.
    pub filesystem_isolation: bool,
    /// Whether the backend isolates environment variables.
    pub environment_isolation: bool,
}
