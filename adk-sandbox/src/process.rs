//! [`ProcessBackend`] — subprocess-based code execution via `tokio::process::Command`.
//!
//! This backend spawns child processes to execute code in various languages.
//! It enforces timeout and environment isolation but does **not** enforce
//! memory limits, network isolation, or filesystem isolation.
//!
//! # Supported Languages
//!
//! | Language   | Execution Strategy                                    |
//! |------------|-------------------------------------------------------|
//! | Rust       | Write to temp file → compile with `rustc` → run binary |
//! | Python     | Write to temp file → run with `python3`               |
//! | JavaScript | Write to temp file → run with `node`                  |
//! | TypeScript | Write to temp file → run with `node` (same as JS)     |
//! | Command    | Execute code as `sh -c "<code>"`                      |
//! | Wasm       | Not supported — use [`WasmBackend`] instead            |
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_sandbox::{ProcessBackend, ExecRequest, Language, SandboxBackend};
//! use std::time::Duration;
//! use std::collections::HashMap;
//!
//! let backend = ProcessBackend::default();
//! let request = ExecRequest {
//!     language: Language::Python,
//!     code: "print('hello')".to_string(),
//!     stdin: None,
//!     timeout: Duration::from_secs(30),
//!     memory_limit_mb: None,
//!     env: HashMap::new(),
//! };
//! let result = backend.execute(request).await?;
//! assert_eq!(result.stdout.trim(), "hello");
//! ```

use std::time::Instant;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{Span, instrument};

use crate::backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
use crate::error::SandboxError;
use crate::types::{ExecRequest, ExecResult, Language};

/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_024 * 1_024;

/// Configuration for [`ProcessBackend`].
///
/// Provides paths to language runtimes. Defaults use bare command names
/// that rely on `PATH` resolution.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::ProcessConfig;
///
/// let config = ProcessConfig {
///     rustc_path: "/usr/local/bin/rustc".to_string(),
///     ..ProcessConfig::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    /// Path to the Rust compiler. Default: `"rustc"`.
    pub rustc_path: String,
    /// Path to the Python 3 interpreter. Default: `"python3"`.
    pub python_path: String,
    /// Path to the Node.js runtime. Default: `"node"`.
    pub node_path: String,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            rustc_path: "rustc".to_string(),
            python_path: "python3".to_string(),
            node_path: "node".to_string(),
        }
    }
}

/// Subprocess-based sandbox backend.
///
/// Executes code by spawning child processes with `tokio::process::Command`.
/// Enforces timeout via `tokio::time::timeout` and environment isolation
/// via `env_clear()`. Does **not** enforce memory, network, or filesystem
/// isolation.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::{ProcessBackend, SandboxBackend};
///
/// let backend = ProcessBackend::default();
/// assert_eq!(backend.name(), "process");
/// ```
#[derive(Debug)]
pub struct ProcessBackend {
    config: ProcessConfig,
}

impl ProcessBackend {
    /// Creates a new `ProcessBackend` with the given configuration.
    pub fn new(config: ProcessConfig) -> Self {
        Self { config }
    }
}

impl Default for ProcessBackend {
    fn default() -> Self {
        Self::new(ProcessConfig::default())
    }
}

/// Truncates a byte buffer to at most `max_bytes`, ensuring the result is
/// valid UTF-8 by backing off to the nearest char boundary.
fn truncate_utf8(bytes: Vec<u8>, max_bytes: usize) -> String {
    if bytes.len() <= max_bytes {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let truncated = &bytes[..max_bytes];
    // Walk backwards to find a valid UTF-8 boundary.
    let mut end = max_bytes;
    while end > 0 && std::str::from_utf8(&truncated[..end]).is_err() {
        end -= 1;
    }
    std::str::from_utf8(&bytes[..end]).unwrap_or("").to_string()
}

#[async_trait]
impl SandboxBackend for ProcessBackend {
    fn name(&self) -> &str {
        "process"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supported_languages: vec![
                Language::Rust,
                Language::Python,
                Language::JavaScript,
                Language::TypeScript,
                Language::Command,
            ],
            isolation_class: "process".to_string(),
            enforced_limits: EnforcedLimits {
                timeout: true,
                memory: false,
                network_isolation: false,
                filesystem_isolation: false,
                environment_isolation: true,
            },
        }
    }

    #[instrument(
        skip_all,
        fields(
            backend = "process",
            language = %request.language,
            exit_code,
            duration_ms,
        )
    )]
    async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError> {
        if let Some(limit) = request.memory_limit_mb {
            tracing::debug!(
                memory_limit_mb = limit,
                "memory limit not enforced by process backend"
            );
        }

        match request.language {
            Language::Rust => self.execute_rust(&request).await,
            Language::Python => self.execute_python(&request).await,
            Language::JavaScript | Language::TypeScript => self.execute_javascript(&request).await,
            Language::Command => self.execute_command(&request).await,
            Language::Wasm => Err(SandboxError::InvalidRequest(
                "Wasm execution is not supported by ProcessBackend. Use WasmBackend instead."
                    .to_string(),
            )),
        }
    }
}

impl ProcessBackend {
    /// Executes Rust code: write to temp file → compile with rustc → run binary.
    async fn execute_rust(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let dir = tempfile::tempdir()?;
        let src_path = dir.path().join("main.rs");
        let bin_path = dir.path().join("main");

        std::fs::write(&src_path, &request.code)?;

        // Compile step
        let compile_output = {
            let mut cmd = Command::new(&self.config.rustc_path);
            cmd.arg(&src_path).arg("-o").arg(&bin_path).env_clear().kill_on_drop(true);
            for (k, v) in &request.env {
                cmd.env(k, v);
            }
            cmd.output().await?
        };

        if !compile_output.status.success() {
            let stderr = truncate_utf8(compile_output.stderr, MAX_OUTPUT_BYTES);
            let stdout = truncate_utf8(compile_output.stdout, MAX_OUTPUT_BYTES);
            let exit_code = compile_output.status.code().unwrap_or(1);
            let result =
                ExecResult { stdout, stderr, exit_code, duration: std::time::Duration::ZERO };
            Span::current().record("exit_code", exit_code);
            Span::current().record("duration_ms", 0_u64);
            return Ok(result);
        }

        // Run the compiled binary
        self.run_binary(&bin_path, request).await
    }

    /// Executes Python code: write to temp file → run with python3.
    async fn execute_python(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let dir = tempfile::tempdir()?;
        let src_path = dir.path().join("script.py");
        std::fs::write(&src_path, &request.code)?;

        let mut cmd = Command::new(&self.config.python_path);
        cmd.arg(&src_path);
        self.run_command(cmd, request).await
    }

    /// Executes JavaScript code: write to temp file → run with node.
    async fn execute_javascript(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let dir = tempfile::tempdir()?;
        let src_path = dir.path().join("script.js");
        std::fs::write(&src_path, &request.code)?;

        let mut cmd = Command::new(&self.config.node_path);
        cmd.arg(&src_path);
        self.run_command(cmd, request).await
    }

    /// Executes a raw shell command via `sh -c`.
    async fn execute_command(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&request.code);
        self.run_command(cmd, request).await
    }

    /// Runs a compiled binary with timeout, env isolation, and stdin piping.
    async fn run_binary(
        &self,
        bin_path: &std::path::Path,
        request: &ExecRequest,
    ) -> Result<ExecResult, SandboxError> {
        let cmd = Command::new(bin_path);
        self.run_command(cmd, request).await
    }

    /// Shared execution logic: env isolation, stdin piping, timeout, output capture.
    async fn run_command(
        &self,
        mut cmd: Command,
        request: &ExecRequest,
    ) -> Result<ExecResult, SandboxError> {
        cmd.env_clear();
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        cmd.kill_on_drop(true);

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if request.stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }

        let start = Instant::now();
        let mut child = cmd.spawn()?;

        // Pipe stdin if provided
        if let Some(ref input) = request.stdin {
            if let Some(mut stdin_handle) = child.stdin.take() {
                stdin_handle.write_all(input.as_bytes()).await?;
                drop(stdin_handle);
            }
        }

        // Wait with timeout
        let output = tokio::time::timeout(request.timeout, child.wait_with_output()).await;
        let duration = start.elapsed();

        match output {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = truncate_utf8(output.stdout, MAX_OUTPUT_BYTES);
                let stderr = truncate_utf8(output.stderr, MAX_OUTPUT_BYTES);

                Span::current().record("exit_code", exit_code);
                Span::current().record("duration_ms", duration.as_millis() as u64);

                Ok(ExecResult { stdout, stderr, exit_code, duration })
            }
            Ok(Err(e)) => {
                Err(SandboxError::ExecutionFailed(format!("failed to wait for child process: {e}")))
            }
            Err(_) => {
                // Timeout — child is killed by kill_on_drop
                Span::current().record("duration_ms", duration.as_millis() as u64);
                Err(SandboxError::Timeout { timeout: request.timeout })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn make_request(language: Language, code: &str) -> ExecRequest {
        let mut env = HashMap::new();
        // ProcessBackend clears the environment (REQ-SBX-023), so tests that
        // invoke interpreters by name need PATH to resolve them.
        if let Ok(path) = std::env::var("PATH") {
            env.insert("PATH".to_string(), path);
        }
        ExecRequest {
            language,
            code: code.to_string(),
            stdin: None,
            timeout: Duration::from_secs(30),
            memory_limit_mb: None,
            env,
        }
    }

    #[tokio::test]
    async fn test_python_execution() {
        let backend = ProcessBackend::default();
        let request = make_request(Language::Python, "print('hello')");
        let result = backend.execute(request).await.unwrap();
        assert!(result.stdout.contains("hello"), "stdout: {}", result.stdout);
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_javascript_execution() {
        let backend = ProcessBackend::default();
        let request = make_request(Language::JavaScript, "console.log('hello')");
        let result = backend.execute(request).await.unwrap();
        assert!(result.stdout.contains("hello"), "stdout: {}", result.stdout);
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_command_execution() {
        let backend = ProcessBackend::default();
        let request = make_request(Language::Command, "echo hello");
        let result = backend.execute(request).await.unwrap();
        assert!(result.stdout.contains("hello"), "stdout: {}", result.stdout);
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_timeout_enforcement() {
        let backend = ProcessBackend::default();
        let request = ExecRequest {
            language: Language::Command,
            code: "sleep 10".to_string(),
            stdin: None,
            timeout: Duration::from_secs(1),
            memory_limit_mb: None,
            env: HashMap::new(),
        };
        let result = backend.execute(request).await;
        assert!(
            matches!(result, Err(SandboxError::Timeout { .. })),
            "expected Timeout, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_environment_isolation() {
        let backend = ProcessBackend::default();
        let mut env = HashMap::new();
        env.insert("MY_TEST_VAR".to_string(), "test_value".to_string());
        let request = ExecRequest {
            language: Language::Command,
            // Use absolute path to env since PATH won't be set
            code: "/usr/bin/env".to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: None,
            env,
        };
        let result = backend.execute(request).await.unwrap();
        // The only env var should be MY_TEST_VAR
        assert!(result.stdout.contains("MY_TEST_VAR=test_value"), "stdout: {}", result.stdout);
        // Common inherited vars like HOME should NOT be present
        assert!(
            !result.stdout.contains("HOME="),
            "HOME should not be inherited: {}",
            result.stdout
        );
    }

    #[tokio::test]
    async fn test_nonzero_exit_code() {
        let backend = ProcessBackend::default();
        let request = make_request(Language::Command, "exit 42");
        let result = backend.execute(request).await.unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn test_wasm_returns_invalid_request() {
        let backend = ProcessBackend::default();
        let request = make_request(Language::Wasm, "");
        let result = backend.execute(request).await;
        assert!(
            matches!(result, Err(SandboxError::InvalidRequest(_))),
            "expected InvalidRequest, got: {result:?}"
        );
    }

    #[test]
    fn test_truncate_utf8_within_limit() {
        let data = "hello world".as_bytes().to_vec();
        let result = truncate_utf8(data, 1024);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_truncate_utf8_at_boundary() {
        // Multi-byte UTF-8: "é" is 2 bytes (0xC3 0xA9)
        let data = "café".as_bytes().to_vec(); // 5 bytes: c a f 0xC3 0xA9
        // Truncate at 4 bytes — would split the "é"
        let result = truncate_utf8(data, 4);
        assert_eq!(result, "caf");
    }

    #[test]
    fn test_capabilities() {
        let backend = ProcessBackend::default();
        let caps = backend.capabilities();
        assert_eq!(caps.isolation_class, "process");
        assert!(caps.enforced_limits.timeout);
        assert!(caps.enforced_limits.environment_isolation);
        assert!(!caps.enforced_limits.memory);
        assert!(!caps.enforced_limits.network_isolation);
        assert!(!caps.enforced_limits.filesystem_isolation);
        assert!(caps.supported_languages.contains(&Language::Rust));
        assert!(caps.supported_languages.contains(&Language::Python));
        assert!(caps.supported_languages.contains(&Language::JavaScript));
        assert!(caps.supported_languages.contains(&Language::TypeScript));
        assert!(caps.supported_languages.contains(&Language::Command));
        assert!(!caps.supported_languages.contains(&Language::Wasm));
    }

    #[test]
    fn test_name() {
        let backend = ProcessBackend::default();
        assert_eq!(backend.name(), "process");
    }

    #[test]
    fn test_process_config_default() {
        let config = ProcessConfig::default();
        assert_eq!(config.rustc_path, "rustc");
        assert_eq!(config.python_path, "python3");
        assert_eq!(config.node_path, "node");
    }
}
