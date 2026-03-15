//! Async executor trait and shared request validation helpers.
//!
//! [`CodeExecutor`] is the backend interface that all execution backends implement.
//! The module also provides [`validate_policy`] and [`validate_request`] helpers
//! that enforce fail-closed semantics: if a backend cannot enforce a requested
//! sandbox control, execution is rejected before user code runs.
//!
//! # Example
//!
//! ```rust
//! use adk_code::{
//!     BackendCapabilities, ExecutionIsolation, SandboxPolicy, validate_policy,
//! };
//!
//! let caps = BackendCapabilities {
//!     isolation: ExecutionIsolation::ContainerEphemeral,
//!     enforce_network_policy: true,
//!     enforce_filesystem_policy: true,
//!     enforce_environment_policy: true,
//!     enforce_timeout: true,
//!     supports_structured_output: true,
//!     supports_process_execution: false,
//!     supports_persistent_workspace: false,
//!     supports_interactive_sessions: false,
//! };
//!
//! let policy = SandboxPolicy::strict_rust();
//! assert!(validate_policy(&caps, &policy).is_ok());
//! ```

use async_trait::async_trait;

use crate::{
    BackendCapabilities, EnvironmentPolicy, ExecutionError, ExecutionLanguage, ExecutionPayload,
    ExecutionRequest, ExecutionResult, FilesystemPolicy, GuestModuleFormat, NetworkPolicy,
    SandboxPolicy,
};

/// Async trait for code execution backends.
///
/// Backends may optionally implement lifecycle methods ([`start`](Self::start),
/// [`stop`](Self::stop), [`restart`](Self::restart)) for persistent execution
/// environments like containers. The default implementations are no-ops, so
/// simple backends (e.g., host-local `rustc`) work without lifecycle management.
///
/// Backends that support persistent environments should override these methods
/// and report `supports_persistent_workspace: true` in their capabilities.
#[async_trait]
pub trait CodeExecutor: Send + Sync {
    /// Human-readable backend name.
    fn name(&self) -> &str;
    /// The capabilities this backend can enforce.
    fn capabilities(&self) -> BackendCapabilities;
    /// Whether this backend supports the given language.
    fn supports_language(&self, lang: &ExecutionLanguage) -> bool;
    /// Execute a request and return a structured result.
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError>;

    /// Start the execution environment (e.g., create and start a container).
    ///
    /// For persistent backends, this creates the underlying environment and
    /// makes it ready for [`execute`](Self::execute) calls. Calling `execute`
    /// on a started backend reuses the same environment.
    ///
    /// The default implementation is a no-op for backends that don't need
    /// lifecycle management (e.g., host-local compilation).
    async fn start(&self) -> Result<(), ExecutionError> {
        Ok(())
    }

    /// Stop the execution environment and release resources.
    ///
    /// For persistent backends, this stops and removes the underlying
    /// environment (e.g., stops and removes a Docker container). After
    /// `stop`, the backend can be restarted with [`start`](Self::start).
    ///
    /// The default implementation is a no-op.
    async fn stop(&self) -> Result<(), ExecutionError> {
        Ok(())
    }

    /// Restart the execution environment.
    ///
    /// Equivalent to [`stop`](Self::stop) followed by [`start`](Self::start),
    /// but backends may implement this more efficiently (e.g., `docker restart`).
    ///
    /// The default implementation calls `stop` then `start`.
    async fn restart(&self) -> Result<(), ExecutionError> {
        self.stop().await?;
        self.start().await
    }

    /// Whether the execution environment is currently running.
    ///
    /// Returns `true` if [`start`](Self::start) has been called and
    /// [`stop`](Self::stop) has not. For backends without lifecycle
    /// management, this always returns `true`.
    async fn is_running(&self) -> bool {
        true
    }
}

/// Validates that the backend can enforce the requested sandbox policy.
///
/// Returns `Err(ExecutionError::UnsupportedPolicy(...))` if any requested
/// control cannot be enforced by the backend. This implements fail-closed
/// semantics: execution is rejected before user code runs.
///
/// # Checks
///
/// - Network policy: if disabled, backend must be able to enforce it
/// - Filesystem policy: if any access is requested, backend must enforce it
/// - Environment policy: if any variables are exposed, backend must enforce it
/// - Timeout: backend must always be able to enforce timeouts
pub fn validate_policy(
    capabilities: &BackendCapabilities,
    policy: &SandboxPolicy,
) -> Result<(), ExecutionError> {
    if matches!(policy.network, NetworkPolicy::Disabled) && !capabilities.enforce_network_policy {
        return Err(ExecutionError::UnsupportedPolicy(
            "backend cannot enforce network restrictions".to_string(),
        ));
    }
    if !matches!(policy.filesystem, FilesystemPolicy::None)
        && !capabilities.enforce_filesystem_policy
    {
        return Err(ExecutionError::UnsupportedPolicy(
            "backend cannot enforce filesystem restrictions".to_string(),
        ));
    }
    if !matches!(policy.environment, EnvironmentPolicy::None)
        && !capabilities.enforce_environment_policy
    {
        return Err(ExecutionError::UnsupportedPolicy(
            "backend cannot enforce environment variable restrictions".to_string(),
        ));
    }
    if !capabilities.enforce_timeout {
        return Err(ExecutionError::UnsupportedPolicy(
            "backend cannot enforce execution timeouts".to_string(),
        ));
    }
    Ok(())
}

/// Validates a full execution request against a backend's capabilities.
///
/// Checks that:
/// 1. The backend supports the requested language
/// 2. The payload type matches the language (e.g., `GuestModule` only for Wasm)
/// 3. The sandbox policy is enforceable by the backend
///
/// Call this before [`CodeExecutor::execute`] for clear, early errors.
///
/// # Example
///
/// ```rust
/// use adk_code::{
///     BackendCapabilities, ExecutionIsolation, ExecutionLanguage,
///     ExecutionPayload, ExecutionRequest, SandboxPolicy,
///     validate_request,
/// };
///
/// let caps = BackendCapabilities {
///     isolation: ExecutionIsolation::ContainerEphemeral,
///     enforce_network_policy: true,
///     enforce_filesystem_policy: true,
///     enforce_environment_policy: true,
///     enforce_timeout: true,
///     supports_structured_output: true,
///     supports_process_execution: false,
///     supports_persistent_workspace: false,
///     supports_interactive_sessions: false,
/// };
///
/// let request = ExecutionRequest {
///     language: ExecutionLanguage::Rust,
///     payload: ExecutionPayload::Source {
///         code: "fn run(input: serde_json::Value) -> serde_json::Value { input }".to_string(),
///     },
///     argv: vec![],
///     stdin: None,
///     input: None,
///     sandbox: SandboxPolicy::strict_rust(),
///     identity: None,
/// };
///
/// let supported = [ExecutionLanguage::Rust];
/// assert!(validate_request(&caps, &supported, &request).is_ok());
/// ```
pub fn validate_request(
    capabilities: &BackendCapabilities,
    supported_languages: &[ExecutionLanguage],
    request: &ExecutionRequest,
) -> Result<(), ExecutionError> {
    // 1. Language support check
    if !supported_languages.contains(&request.language) {
        return Err(ExecutionError::UnsupportedLanguage(format!("{}", request.language)));
    }

    // 2. Payload-language compatibility check
    match (&request.language, &request.payload) {
        // GuestModule payloads are only valid for Wasm
        (lang, ExecutionPayload::GuestModule { format, .. }) => match format {
            GuestModuleFormat::Wasm if *lang != ExecutionLanguage::Wasm => {
                return Err(ExecutionError::InvalidRequest(format!(
                    "GuestModule(Wasm) payload requires Wasm language, got {lang}"
                )));
            }
            _ => {}
        },
        // Wasm language requires a GuestModule payload
        (ExecutionLanguage::Wasm, ExecutionPayload::Source { .. }) => {
            return Err(ExecutionError::InvalidRequest(
                "Wasm language requires a GuestModule payload, not Source".to_string(),
            ));
        }
        _ => {}
    }

    // 3. Policy enforcement check
    validate_policy(capabilities, &request.sandbox)?;

    Ok(())
}
