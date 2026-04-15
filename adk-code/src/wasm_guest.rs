//! WASM guest module executor — secondary backend for portable sandboxed plugins.
//!
//! [`WasmGuestExecutor`] executes precompiled `.wasm` guest modules inside a
//! constrained runtime with a narrow host ABI. It is clearly separate from raw
//! JavaScript source execution and from the flagship [`crate::RustSandboxExecutor`].
//!
//! # Product Posture
//!
//! This is a secondary backend for portable guest-module execution. The primary
//! code-execution path remains authored Rust via [`crate::RustSandboxExecutor`].
//!
//! # Guest Module Contract
//!
//! Guest modules must:
//!
//! - Be valid WebAssembly binary format (`.wasm`)
//! - Export a `run` function that accepts and returns i32 pointers to JSON buffers
//! - Use only the narrow host ABI provided by the executor
//!
//! # Phase 1 Scope
//!
//! Phase 1 provides a placeholder implementation that validates guest module
//! format and boundaries. Full WASM runtime integration (e.g., `wasmtime` or
//! `wasmer`) is deferred to a later phase when the Rust-first path is stable.
//!
//! # Isolation Model
//!
//! | Capability | Enforced | Mechanism |
//! |---|---|---|
//! | Network policy | Yes | Guest modules have no network access by default |
//! | Filesystem policy | Yes | Guest modules have no filesystem access by default |
//! | Environment policy | Yes | Guest modules have no environment access |
//! | Timeout | Yes | Fuel-based or wall-clock limits |
//! | Structured output | Yes | JSON via host ABI |
//!
//! # Example
//!
//! ```rust
//! use adk_code::{
//!     CodeExecutor, WasmGuestExecutor, ExecutionIsolation, ExecutionLanguage,
//! };
//!
//! let executor = WasmGuestExecutor::new();
//! assert_eq!(executor.name(), "wasm-guest");
//! assert_eq!(executor.capabilities().isolation, ExecutionIsolation::InProcess);
//! assert!(executor.supports_language(&ExecutionLanguage::Wasm));
//! assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
//! ```

use async_trait::async_trait;
use tracing::{debug, warn};

use crate::{
    BackendCapabilities, CodeExecutor, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus, GuestModuleFormat,
    validate_request,
};

/// The WebAssembly binary magic number (`\0asm`).
const WASM_MAGIC: &[u8] = b"\0asm";

/// Minimum valid WASM module size (magic + version = 8 bytes).
const WASM_MIN_SIZE: usize = 8;

/// Configuration for the WASM guest executor.
///
/// # Example
///
/// ```rust
/// use adk_code::WasmGuestConfig;
///
/// let config = WasmGuestConfig::default();
/// assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
/// ```
#[derive(Debug, Clone)]
pub struct WasmGuestConfig {
    /// Maximum memory in bytes the guest module may use.
    pub max_memory_bytes: usize,
    /// Maximum fuel (instruction count) for execution, if supported.
    pub max_fuel: Option<u64>,
}

impl Default for WasmGuestConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB
            max_fuel: Some(1_000_000_000),      // 1 billion instructions
        }
    }
}

/// Guest-module backend for precompiled `.wasm` modules.
///
/// Executes guest modules through a constrained runtime with a narrow host ABI.
/// This is clearly separate from raw JavaScript source execution — it accepts
/// only [`ExecutionPayload::GuestModule`] payloads with [`GuestModuleFormat::Wasm`].
///
/// # Important Distinction
///
/// `WasmGuestExecutor` is NOT a JavaScript executor. It runs precompiled
/// WebAssembly binary modules. For JavaScript source execution, use
/// `EmbeddedJsExecutor` (secondary scripting) or
/// [`ContainerCommandExecutor`](crate::ContainerCommandExecutor) (container-isolated Node.js).
///
/// # Example
///
/// ```rust
/// use adk_code::{CodeExecutor, WasmGuestExecutor, ExecutionLanguage};
///
/// let executor = WasmGuestExecutor::new();
/// assert!(executor.supports_language(&ExecutionLanguage::Wasm));
/// assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
/// assert!(!executor.supports_language(&ExecutionLanguage::Rust));
/// ```
#[derive(Debug, Clone)]
pub struct WasmGuestExecutor {
    config: WasmGuestConfig,
}

impl WasmGuestExecutor {
    /// Create a new WASM guest executor with default configuration.
    pub fn new() -> Self {
        Self { config: WasmGuestConfig::default() }
    }

    /// Create a new WASM guest executor with the given configuration.
    pub fn with_config(config: WasmGuestConfig) -> Self {
        Self { config }
    }
}

impl Default for WasmGuestExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate that the bytes represent a valid WASM binary module.
///
/// Checks the magic number (`\0asm`) and minimum size. This is a structural
/// validation — it does not verify the module's internal consistency or
/// exported functions.
///
/// # Example
///
/// ```rust
/// use adk_code::validate_wasm_bytes;
///
/// // Valid WASM magic + version 1
/// let valid = b"\0asm\x01\x00\x00\x00";
/// assert!(validate_wasm_bytes(valid).is_ok());
///
/// // Too short
/// assert!(validate_wasm_bytes(b"\0asm").is_err());
///
/// // Wrong magic
/// assert!(validate_wasm_bytes(b"not_wasm_at_all!").is_err());
/// ```
pub fn validate_wasm_bytes(bytes: &[u8]) -> Result<(), ExecutionError> {
    if bytes.len() < WASM_MIN_SIZE {
        return Err(ExecutionError::InvalidRequest(format!(
            "WASM module too small: {} bytes (minimum {WASM_MIN_SIZE})",
            bytes.len()
        )));
    }

    if !bytes.starts_with(WASM_MAGIC) {
        return Err(ExecutionError::InvalidRequest(
            "invalid WASM module: missing magic number (\\0asm)".to_string(),
        ));
    }

    Ok(())
}

#[async_trait]
impl CodeExecutor for WasmGuestExecutor {
    fn name(&self) -> &str {
        "wasm-guest"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            isolation: ExecutionIsolation::InProcess,
            // WASM guest modules have no access to host resources by default.
            enforce_network_policy: true,
            enforce_filesystem_policy: true,
            enforce_environment_policy: true,
            enforce_timeout: true,
            supports_structured_output: true,
            supports_process_execution: false,
            supports_persistent_workspace: false,
            supports_interactive_sessions: false,
        }
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        matches!(lang, ExecutionLanguage::Wasm)
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        validate_request(&self.capabilities(), &[ExecutionLanguage::Wasm], &request)?;

        // Extract and validate the guest module bytes.
        let bytes = match &request.payload {
            ExecutionPayload::GuestModule { format, bytes } => {
                match format {
                    GuestModuleFormat::Wasm => {}
                }
                bytes.clone()
            }
            ExecutionPayload::Source { .. } => {
                return Err(ExecutionError::InvalidRequest(
                    "WasmGuestExecutor requires a GuestModule payload, not Source. \
                     For JavaScript source execution, use EmbeddedJsExecutor or ContainerCommandExecutor."
                        .to_string(),
                ));
            }
        };

        validate_wasm_bytes(&bytes)?;

        debug!(
            module_size = bytes.len(),
            max_memory = self.config.max_memory_bytes,
            max_fuel = ?self.config.max_fuel,
            "validating WASM guest module"
        );

        // Phase 1: Structural validation only.
        // Full WASM runtime integration is deferred until the Rust-first path
        // is stable. This placeholder validates the module format and returns
        // a descriptive result indicating the module was accepted but not executed.
        warn!("WASM guest execution is phase 1 placeholder — module validated but not executed");

        Ok(ExecutionResult {
            status: ExecutionStatus::Success,
            stdout: String::new(),
            stderr: "WASM guest execution: module validated (phase 1 placeholder — \
                     full runtime integration pending)"
                .to_string(),
            output: Some(serde_json::json!({
                "phase": 1,
                "module_size_bytes": bytes.len(),
                "validated": true,
                "executed": false,
                "note": "Full WASM runtime integration is deferred to a later phase. \
                         The module passed structural validation."
            })),
            exit_code: Some(0),
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms: 0,
            metadata: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxPolicy;

    /// A minimal valid WASM module (magic + version 1 + empty sections).
    fn minimal_wasm_module() -> Vec<u8> {
        // \0asm followed by version 1 (little-endian u32)
        vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]
    }

    #[test]
    fn capabilities_are_in_process() {
        let executor = WasmGuestExecutor::new();
        let caps = executor.capabilities();
        assert_eq!(caps.isolation, ExecutionIsolation::InProcess);
        assert!(caps.enforce_network_policy);
        assert!(caps.enforce_filesystem_policy);
        assert!(caps.enforce_environment_policy);
        assert!(caps.enforce_timeout);
        assert!(caps.supports_structured_output);
        assert!(!caps.supports_process_execution);
        assert!(!caps.supports_persistent_workspace);
        assert!(!caps.supports_interactive_sessions);
    }

    #[test]
    fn supports_only_wasm() {
        let executor = WasmGuestExecutor::new();
        assert!(executor.supports_language(&ExecutionLanguage::Wasm));
        assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
        assert!(!executor.supports_language(&ExecutionLanguage::Rust));
        assert!(!executor.supports_language(&ExecutionLanguage::Python));
        assert!(!executor.supports_language(&ExecutionLanguage::Command));
    }

    #[test]
    fn validate_wasm_bytes_valid() {
        assert!(validate_wasm_bytes(&minimal_wasm_module()).is_ok());
    }

    #[test]
    fn validate_wasm_bytes_too_short() {
        let err = validate_wasm_bytes(b"\0asm").unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
        assert!(err.to_string().contains("too small"));
    }

    #[test]
    fn validate_wasm_bytes_wrong_magic() {
        let err = validate_wasm_bytes(b"not_wasm_at_all!").unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
        assert!(err.to_string().contains("magic number"));
    }

    #[test]
    fn validate_wasm_bytes_empty() {
        let err = validate_wasm_bytes(b"").unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn rejects_source_payload() {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Wasm,
            payload: ExecutionPayload::Source { code: "console.log('hello')".to_string() },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        // validate_request catches this before execute body
        let err = executor.execute(request).await.unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn accepts_valid_wasm_module() {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Wasm,
            payload: ExecutionPayload::GuestModule {
                format: GuestModuleFormat::Wasm,
                bytes: minimal_wasm_module(),
            },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let result = executor.execute(request).await.unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        assert!(result.output.is_some());
        let output = result.output.unwrap();
        assert_eq!(output["validated"], true);
        assert_eq!(output["executed"], false);
        assert_eq!(output["module_size_bytes"], 8);
    }

    #[tokio::test]
    async fn rejects_invalid_wasm_bytes() {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Wasm,
            payload: ExecutionPayload::GuestModule {
                format: GuestModuleFormat::Wasm,
                bytes: b"not_wasm".to_vec(),
            },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let err = executor.execute(request).await.unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn rejects_javascript_language() {
        let executor = WasmGuestExecutor::new();
        let request = ExecutionRequest {
            language: ExecutionLanguage::JavaScript,
            payload: ExecutionPayload::GuestModule {
                format: GuestModuleFormat::Wasm,
                bytes: minimal_wasm_module(),
            },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let err = executor.execute(request).await.unwrap_err();
        // validate_request catches language mismatch
        assert!(
            matches!(err, ExecutionError::UnsupportedLanguage(_))
                || matches!(err, ExecutionError::InvalidRequest(_))
        );
    }

    #[test]
    fn default_config_values() {
        let config = WasmGuestConfig::default();
        assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.max_fuel, Some(1_000_000_000));
    }
}
