//! Core execution types for the code execution substrate.
//!
//! This module defines the typed primitives shared by all execution backends,
//! language-preset tools, and Studio integration:
//!
//! - [`ExecutionLanguage`] — supported execution languages
//! - [`ExecutionPayload`] — source code or guest module bytes
//! - [`ExecutionIsolation`] — backend isolation class
//! - [`SandboxPolicy`] — requested sandbox controls
//! - [`BackendCapabilities`] — what a backend can actually enforce
//! - [`ExecutionRequest`] — full execution request
//! - [`ExecutionResult`] — structured execution outcome
//! - [`ExecutionStatus`] — terminal execution status

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// One megabyte in bytes, used as the default stdout/stderr limit.
const ONE_MB: usize = 1_048_576;

/// Supported execution languages.
///
/// `Rust` is the primary first-class language. Other languages are available
/// through appropriate backends.
///
/// # Example
///
/// ```rust
/// use adk_code::ExecutionLanguage;
///
/// let lang = ExecutionLanguage::Rust;
/// assert_eq!(lang, ExecutionLanguage::Rust);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionLanguage {
    /// Rust — the primary authored-code path.
    Rust,
    /// JavaScript — secondary scripting and transform support.
    JavaScript,
    /// WebAssembly guest module execution.
    Wasm,
    /// Python — container-backed execution.
    Python,
    /// Raw command execution (shell, interpreter, etc.).
    Command,
}

impl std::fmt::Display for ExecutionLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => write!(f, "Rust"),
            Self::JavaScript => write!(f, "JavaScript"),
            Self::Wasm => write!(f, "Wasm"),
            Self::Python => write!(f, "Python"),
            Self::Command => write!(f, "Command"),
        }
    }
}

/// Format of a precompiled guest module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuestModuleFormat {
    /// WebAssembly binary format.
    Wasm,
}

/// The code or module to execute.
///
/// Source payloads carry inline code strings. Guest module payloads carry
/// precompiled binary modules (e.g., `.wasm` files).
///
/// # Example
///
/// ```rust
/// use adk_code::ExecutionPayload;
///
/// let payload = ExecutionPayload::Source {
///     code: "fn run(input: serde_json::Value) -> serde_json::Value { input }".to_string(),
/// };
/// ```
#[derive(Debug, Clone)]
pub enum ExecutionPayload {
    /// Inline source code to compile and/or interpret.
    Source {
        /// The source code string.
        code: String,
    },
    /// A precompiled guest module (e.g., WASM).
    GuestModule {
        /// The binary format of the guest module.
        format: GuestModuleFormat,
        /// The raw module bytes.
        bytes: Vec<u8>,
    },
}

/// Backend isolation class.
///
/// Makes the isolation model explicit so that host-local and container-backed
/// execution cannot be presented as equivalent.
///
/// # Example
///
/// ```rust
/// use adk_code::ExecutionIsolation;
///
/// let iso = ExecutionIsolation::ContainerEphemeral;
/// assert_ne!(iso, ExecutionIsolation::HostLocal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionIsolation {
    /// Execution runs in the same process (e.g., embedded JS engine).
    InProcess,
    /// Execution runs as a local host process without strong OS isolation.
    HostLocal,
    /// Execution runs in an ephemeral container destroyed after completion.
    ContainerEphemeral,
    /// Execution runs in a persistent container that survives across requests.
    ContainerPersistent,
    /// Execution runs on a remote provider-hosted service.
    ProviderHosted,
}

/// Network access policy for sandboxed execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetworkPolicy {
    /// No network access allowed.
    Disabled,
    /// Network access is permitted.
    Enabled,
}

/// Filesystem access policy for sandboxed execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesystemPolicy {
    /// No filesystem access.
    None,
    /// Read-only access to a workspace root.
    WorkspaceReadOnly {
        /// The workspace root path.
        root: PathBuf,
    },
    /// Read-write access to a workspace root.
    WorkspaceReadWrite {
        /// The workspace root path.
        root: PathBuf,
    },
    /// Explicit path-level access control.
    Paths {
        /// Paths with read-only access.
        read_only: Vec<PathBuf>,
        /// Paths with read-write access.
        read_write: Vec<PathBuf>,
    },
}

/// Environment variable access policy for sandboxed execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentPolicy {
    /// No environment variables exposed.
    None,
    /// Only the listed environment variable names are exposed.
    AllowList(Vec<String>),
}

/// Sandbox policy describing the requested execution constraints.
///
/// Backends compare this policy against their [`BackendCapabilities`] and
/// reject execution if they cannot enforce a requested control.
///
/// # Example
///
/// ```rust
/// use adk_code::SandboxPolicy;
///
/// let policy = SandboxPolicy::strict_rust();
/// assert_eq!(policy.max_stdout_bytes, 1_048_576);
/// ```
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Network access policy.
    pub network: NetworkPolicy,
    /// Filesystem access policy.
    pub filesystem: FilesystemPolicy,
    /// Environment variable access policy.
    pub environment: EnvironmentPolicy,
    /// Maximum execution duration.
    pub timeout: Duration,
    /// Maximum bytes captured from stdout before truncation.
    pub max_stdout_bytes: usize,
    /// Maximum bytes captured from stderr before truncation.
    pub max_stderr_bytes: usize,
    /// Working directory for execution, if any.
    pub working_directory: Option<PathBuf>,
}

impl SandboxPolicy {
    /// Strict policy for Rust sandbox execution.
    ///
    /// - No network access
    /// - No filesystem access
    /// - No environment variables
    /// - 30-second timeout
    /// - 1 MB stdout/stderr limits
    pub fn strict_rust() -> Self {
        Self {
            network: NetworkPolicy::Disabled,
            filesystem: FilesystemPolicy::None,
            environment: EnvironmentPolicy::None,
            timeout: Duration::from_secs(30),
            max_stdout_bytes: ONE_MB,
            max_stderr_bytes: ONE_MB,
            working_directory: None,
        }
    }

    /// Host-local policy for backends that run on the host without isolation.
    ///
    /// Unlike [`strict_rust`](Self::strict_rust), this policy uses
    /// `NetworkPolicy::Enabled` and `FilesystemPolicy::None` so that
    /// host-local backends (which cannot enforce network or filesystem
    /// restrictions) pass policy validation. The trade-off is that the
    /// executed code has the same network and filesystem access as the
    /// host process.
    ///
    /// - Network access: allowed (host-local cannot restrict)
    /// - Filesystem access: none requested
    /// - Environment variables: none exposed
    /// - 30-second timeout
    /// - 1 MB stdout/stderr limits
    pub fn host_local() -> Self {
        Self {
            network: NetworkPolicy::Enabled,
            filesystem: FilesystemPolicy::None,
            environment: EnvironmentPolicy::None,
            timeout: Duration::from_secs(30),
            max_stdout_bytes: ONE_MB,
            max_stderr_bytes: ONE_MB,
            working_directory: None,
        }
    }

    /// Strict policy for embedded JavaScript execution.
    ///
    /// Same defaults as Rust but with a shorter 5-second timeout,
    /// appropriate for lightweight transforms and scripting.
    pub fn strict_js() -> Self {
        Self {
            network: NetworkPolicy::Disabled,
            filesystem: FilesystemPolicy::None,
            environment: EnvironmentPolicy::None,
            timeout: Duration::from_secs(5),
            max_stdout_bytes: ONE_MB,
            max_stderr_bytes: ONE_MB,
            working_directory: None,
        }
    }
}

impl Default for SandboxPolicy {
    /// Sensible defaults: no network, no filesystem, no env vars, 30s timeout, 1 MB limits.
    fn default() -> Self {
        Self::strict_rust()
    }
}

/// Capabilities that a backend can actually enforce.
///
/// This makes the isolation model explicit so callers and docs can distinguish
/// what a backend claims from what it can guarantee.
///
/// # Example
///
/// ```rust
/// use adk_code::{BackendCapabilities, ExecutionIsolation};
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
/// assert!(caps.enforce_network_policy);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// The isolation class this backend provides.
    pub isolation: ExecutionIsolation,
    /// Whether the backend can enforce network restrictions.
    pub enforce_network_policy: bool,
    /// Whether the backend can enforce filesystem restrictions.
    pub enforce_filesystem_policy: bool,
    /// Whether the backend can enforce environment variable restrictions.
    pub enforce_environment_policy: bool,
    /// Whether the backend can enforce execution timeouts.
    pub enforce_timeout: bool,
    /// Whether the backend supports structured JSON output.
    pub supports_structured_output: bool,
    /// Whether the backend supports spawning child processes.
    pub supports_process_execution: bool,
    /// Whether the backend supports persistent workspaces across requests.
    pub supports_persistent_workspace: bool,
    /// Whether the backend supports interactive/REPL-style sessions.
    pub supports_interactive_sessions: bool,
}

/// A full execution request.
///
/// Combines language, payload, sandbox policy, optional I/O, and identity
/// into a single typed request that backends can validate and execute.
///
/// # Example
///
/// ```rust
/// use adk_code::{ExecutionRequest, ExecutionLanguage, ExecutionPayload, SandboxPolicy};
///
/// let request = ExecutionRequest {
///     language: ExecutionLanguage::Rust,
///     payload: ExecutionPayload::Source {
///         code: r#"fn run(input: serde_json::Value) -> serde_json::Value { input }"#.to_string(),
///     },
///     argv: vec![],
///     stdin: None,
///     input: None,
///     sandbox: SandboxPolicy::strict_rust(),
///     identity: None,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    /// The target execution language.
    pub language: ExecutionLanguage,
    /// The code or module to execute.
    pub payload: ExecutionPayload,
    /// Command-line arguments passed to the executed program.
    pub argv: Vec<String>,
    /// Optional stdin bytes fed to the executed program.
    pub stdin: Option<Vec<u8>>,
    /// Optional structured JSON input injected through a controlled harness.
    pub input: Option<Value>,
    /// The sandbox policy for this execution.
    pub sandbox: SandboxPolicy,
    /// Optional execution identity for audit and telemetry correlation.
    pub identity: Option<String>,
}

/// Terminal status of an execution.
///
/// Distinguishes compile failures from runtime failures, timeouts, and rejections.
///
/// # Example
///
/// ```rust
/// use adk_code::ExecutionStatus;
///
/// let status = ExecutionStatus::CompileFailed;
/// assert_ne!(status, ExecutionStatus::Failed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionStatus {
    /// Execution completed successfully.
    Success,
    /// Execution exceeded the configured timeout.
    Timeout,
    /// Compilation or build step failed (distinct from runtime failure).
    CompileFailed,
    /// Runtime execution failed.
    Failed,
    /// Execution was rejected before running (policy or scope check).
    Rejected,
}

/// Execution metadata for telemetry, audit, and artifact correlation.
///
/// Captures backend name, language, duration, status, and correlation identity
/// so that executions can be traced and audited across sessions and invocations.
///
/// # Example
///
/// ```rust
/// use adk_code::{ExecutionMetadata, ExecutionLanguage, ExecutionStatus, ExecutionIsolation};
///
/// let meta = ExecutionMetadata {
///     backend_name: "rust-sandbox".to_string(),
///     language: ExecutionLanguage::Rust,
///     isolation: ExecutionIsolation::HostLocal,
///     status: ExecutionStatus::Success,
///     duration_ms: 42,
///     identity: Some("inv-123".to_string()),
///     artifact_refs: vec![],
/// };
/// assert_eq!(meta.backend_name, "rust-sandbox");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionMetadata {
    /// Name of the backend that executed the request.
    pub backend_name: String,
    /// Language that was executed.
    pub language: ExecutionLanguage,
    /// Isolation class of the backend.
    pub isolation: ExecutionIsolation,
    /// Terminal execution status.
    pub status: ExecutionStatus,
    /// Execution wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Correlation identity (invocation ID, session ID, etc.) when available.
    pub identity: Option<String>,
    /// References to artifacts stored externally (e.g., large outputs).
    pub artifact_refs: Vec<ArtifactRef>,
}

/// Reference to an externally stored artifact.
///
/// When execution output exceeds inline size limits, the result can reference
/// artifacts stored through ADK artifact mechanisms instead of forcing large
/// binary data into inline JSON strings.
///
/// # Example
///
/// ```rust
/// use adk_code::ArtifactRef;
///
/// let artifact = ArtifactRef {
///     key: "stdout-full".to_string(),
///     size_bytes: 2_000_000,
///     content_type: Some("text/plain".to_string()),
/// };
/// assert_eq!(artifact.key, "stdout-full");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    /// Artifact storage key or identifier.
    pub key: String,
    /// Size of the artifact in bytes.
    pub size_bytes: u64,
    /// MIME content type, if known.
    pub content_type: Option<String>,
}

/// Structured result of a code execution.
///
/// Captures stdout, stderr, structured output, truncation flags, exit code,
/// duration, and optional execution metadata so downstream consumers can
/// reason about outcomes reliably.
///
/// # Example
///
/// ```rust
/// use adk_code::{ExecutionResult, ExecutionStatus};
///
/// let result = ExecutionResult {
///     status: ExecutionStatus::Success,
///     stdout: "hello\n".to_string(),
///     stderr: String::new(),
///     output: Some(serde_json::json!({ "answer": 42 })),
///     exit_code: Some(0),
///     stdout_truncated: false,
///     stderr_truncated: false,
///     duration_ms: 37,
///     metadata: None,
/// };
/// assert_eq!(result.status, ExecutionStatus::Success);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResult {
    /// Terminal execution status.
    pub status: ExecutionStatus,
    /// Captured stdout text (may be truncated).
    pub stdout: String,
    /// Captured stderr text (may be truncated).
    pub stderr: String,
    /// Optional structured JSON output from the executed code.
    pub output: Option<Value>,
    /// Process exit code, if available.
    pub exit_code: Option<i32>,
    /// Whether stdout was truncated due to size limits.
    pub stdout_truncated: bool,
    /// Whether stderr was truncated due to size limits.
    pub stderr_truncated: bool,
    /// Execution wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Optional execution metadata for telemetry and audit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ExecutionMetadata>,
}
