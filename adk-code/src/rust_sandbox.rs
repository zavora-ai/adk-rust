//! Rust sandbox executor — the flagship Rust-authored code execution backend.
//!
//! [`RustSandboxExecutor`] compiles and executes authored Rust in an isolated
//! environment using a host-local process approach (phase 1). It wraps user code
//! in a harness that injects JSON input via stdin and captures JSON output from
//! stdout.
//!
//! # Phase 1 Rust Source Model
//!
//! Phase 1 supports **self-contained Rust snippets** compiled into a controlled
//! harness. This is intentionally bounded — the executor does not support
//! arbitrary Cargo workspaces, multi-file projects, or external dependency
//! resolution.
//!
//! ## The `run()` Contract
//!
//! User code must provide exactly one entry point:
//!
//! ```rust,ignore
//! fn run(input: serde_json::Value) -> serde_json::Value
//! ```
//!
//! The harness wraps this function with a generated `fn main()` that:
//!
//! 1. Reads JSON input from stdin
//! 2. Calls the user's `run()` function
//! 3. Writes JSON output to stdout as the last line
//!
//! ## Available Imports and Dependencies
//!
//! The harness automatically provides:
//!
//! | Dependency     | Version | Notes |
//! |----------------|---------|-------|
//! | `serde_json`   | workspace-pinned | Re-exported as `serde_json::Value`, `serde_json::json!`, etc. |
//!
//! The harness injects `use serde_json::Value;` at the top. User code can
//! reference any public item from `serde_json` (e.g., `serde_json::json!`,
//! `serde_json::Map`, `serde_json::Number`).
//!
//! No other external crates are available. The Rust standard library is fully
//! available (`std::collections`, `std::fmt`, etc.).
//!
//! ## What Is NOT Supported (Phase 1)
//!
//! - **`fn main()`**: The harness provides `main()`. User code that defines its
//!   own `fn main()` will be rejected with an [`ExecutionError::InvalidRequest`].
//! - **`Cargo.toml`**: There is no Cargo project. Compilation uses `rustc` directly.
//! - **External crates**: Only `serde_json` is linked. `use some_other_crate::*`
//!   will produce a compile error.
//! - **Multi-file projects**: The source model is a single code string. No `mod`
//!   declarations referencing external files.
//! - **Procedural macros or build scripts**: Not available.
//! - **`#![...]` crate-level attributes**: Not supported in the harness body.
//!
//! ## Phase 1 Isolation Model
//!
//! Phase 1 uses host-local process execution via `rustc`. The backend is honest
//! about its capabilities:
//!
//! - **Timeout enforcement**: Yes (via `tokio::time::timeout`)
//! - **Output truncation**: Yes (configurable limits)
//! - **Network restriction**: No (host-local cannot enforce this)
//! - **Filesystem restriction**: No (host-local cannot enforce this)
//! - **Environment restriction**: No (host-local cannot enforce this)
//!
//! ## Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), adk_code::ExecutionError> {
//! use adk_code::{
//!     CodeExecutor, ExecutionLanguage, ExecutionPayload, ExecutionRequest,
//!     ExecutionStatus, SandboxPolicy, RustSandboxExecutor,
//! };
//!
//! let executor = RustSandboxExecutor::default();
//! let request = ExecutionRequest {
//!     language: ExecutionLanguage::Rust,
//!     payload: ExecutionPayload::Source {
//!         code: r#"
//! fn run(input: serde_json::Value) -> serde_json::Value {
//!     let v = input["value"].as_i64().unwrap_or(0);
//!     serde_json::json!({ "doubled": v * 2 })
//! }
//! "#.to_string(),
//!     },
//!     argv: vec![],
//!     stdin: None,
//!     input: Some(serde_json::json!({ "value": 21 })),
//!     sandbox: SandboxPolicy::default(),
//!     identity: None,
//! };
//!
//! let result = executor.execute(request).await?;
//! assert_eq!(result.status, ExecutionStatus::Success);
//! assert_eq!(result.output, Some(serde_json::json!({ "doubled": 42 })));
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, instrument, warn};

use crate::harness::{
    HARNESS_TEMPLATE, extract_structured_output, truncate_output, validate_rust_source,
};
use crate::{
    BackendCapabilities, CodeExecutor, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus, validate_request,
};

/// Configuration for the Rust sandbox executor.
///
/// # Example
///
/// ```rust
/// use adk_code::RustSandboxConfig;
///
/// let config = RustSandboxConfig::default();
/// assert_eq!(config.rustc_path, "rustc");
/// ```
#[derive(Debug, Clone)]
pub struct RustSandboxConfig {
    /// Path to the `rustc` compiler binary.
    pub rustc_path: String,
    /// Extra flags passed to `rustc` during compilation.
    pub rustc_flags: Vec<String>,
    /// Path to the `serde_json` rlib or directory containing it.
    /// If `None`, the executor will attempt to locate it automatically.
    pub serde_json_path: Option<PathBuf>,
}

impl Default for RustSandboxConfig {
    fn default() -> Self {
        Self { rustc_path: "rustc".to_string(), rustc_flags: vec![], serde_json_path: None }
    }
}

/// The flagship Rust-authored code execution backend.
///
/// Compiles and executes authored Rust using a host-local process approach.
/// Phase 1 is honest about isolation: it can enforce timeouts and output
/// truncation, but cannot enforce network or filesystem restrictions.
///
/// # Backend Capability Reporting
///
/// `RustSandboxExecutor` reports its capabilities truthfully through
/// [`BackendCapabilities`]. The phase 1 implementation uses host-local
/// process execution (`rustc` + spawned binary), so:
///
/// | Capability | Enforced | Reason |
/// |---|---|---|
/// | Isolation class | `HostLocal` | Runs as a local process, not in a container |
/// | Network policy | No | Host-local processes inherit host network access |
/// | Filesystem policy | No | Host-local processes inherit host filesystem access |
/// | Environment policy | No | Host-local processes inherit host environment |
/// | Timeout | Yes | Enforced via `tokio::time::timeout` |
/// | Structured output | Yes | Harness extracts JSON from last stdout line |
/// | Process execution | No | User code cannot spawn child processes through the harness |
/// | Persistent workspace | No | Each execution uses a fresh temp directory |
/// | Interactive sessions | No | Single-shot execution only |
///
/// Callers should use `validate_policy` to check whether a requested
/// `SandboxPolicy` is compatible with these capabilities before execution.
/// If a policy requests a control the backend cannot enforce (e.g., disabled
/// network), validation fails with [`ExecutionError::UnsupportedPolicy`].
///
/// # Example
///
/// ```rust
/// use adk_code::{CodeExecutor, RustSandboxExecutor, ExecutionIsolation};
///
/// let executor = RustSandboxExecutor::default();
/// assert_eq!(executor.name(), "rust-sandbox");
/// assert_eq!(executor.capabilities().isolation, ExecutionIsolation::HostLocal);
/// assert!(executor.capabilities().enforce_timeout);
/// assert!(!executor.capabilities().enforce_network_policy);
/// ```
#[derive(Debug, Clone)]
pub struct RustSandboxExecutor {
    config: RustSandboxConfig,
}

impl RustSandboxExecutor {
    /// Create a new executor with the given configuration.
    pub fn new(config: RustSandboxConfig) -> Self {
        Self { config }
    }
}

impl Default for RustSandboxExecutor {
    fn default() -> Self {
        Self::new(RustSandboxConfig::default())
    }
}

#[async_trait]
impl CodeExecutor for RustSandboxExecutor {
    fn name(&self) -> &str {
        "rust-sandbox"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            isolation: ExecutionIsolation::HostLocal,
            enforce_network_policy: false,
            enforce_filesystem_policy: false,
            enforce_environment_policy: false,
            enforce_timeout: true,
            supports_structured_output: true,
            supports_process_execution: false,
            supports_persistent_workspace: false,
            supports_interactive_sessions: false,
        }
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        matches!(lang, ExecutionLanguage::Rust)
    }

    #[instrument(skip_all, fields(backend = "rust-sandbox", language = "Rust"))]
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        // Validate the request against our capabilities.
        validate_request(&self.capabilities(), &[ExecutionLanguage::Rust], &request)?;

        let code = match &request.payload {
            ExecutionPayload::Source { code } => code.clone(),
            ExecutionPayload::GuestModule { .. } => {
                return Err(ExecutionError::InvalidRequest(
                    "RustSandboxExecutor only accepts Source payloads".to_string(),
                ));
            }
        };

        // Validate that the source fits the phase 1 bounded model.
        validate_rust_source(&code)?;

        let start = Instant::now();

        // Create a temp directory for compilation artifacts.
        let tmp_dir = tempfile::tempdir().map_err(|e| {
            ExecutionError::ExecutionFailed(format!("failed to create temp directory: {e}"))
        })?;

        let source_path = tmp_dir.path().join("main.rs");
        let binary_path = tmp_dir.path().join("main");

        // Write the harnessed source file.
        let harnessed_source = HARNESS_TEMPLATE.replace("{user_code}", &code);
        tokio::fs::write(&source_path, &harnessed_source).await.map_err(|e| {
            ExecutionError::ExecutionFailed(format!("failed to write source file: {e}"))
        })?;

        debug!(source_path = %source_path.display(), "wrote harnessed source");

        // ── Compilation ────────────────────────────────────────────────
        let compile_result = self.compile(&source_path, &binary_path, &request).await?;
        if let Some(result) = compile_result {
            // Compilation failed — return the compile failure result.
            return Ok(result);
        }

        info!("compilation succeeded, executing binary");

        // ── Execution ──────────────────────────────────────────────────
        let result = self.run_binary(&binary_path, &request, start).await;

        // Clean up temp dir (best-effort, tempfile handles this on drop too).
        drop(tmp_dir);

        result
    }
}

impl RustSandboxExecutor {
    /// Compile the source file. Returns `Ok(Some(result))` if compilation failed
    /// (with a `CompileFailed` result), `Ok(None)` if compilation succeeded,
    /// or `Err` for infrastructure failures.
    async fn compile(
        &self,
        source_path: &std::path::Path,
        binary_path: &std::path::Path,
        request: &ExecutionRequest,
    ) -> Result<Option<ExecutionResult>, ExecutionError> {
        let serde_json_dep = self.find_serde_json_dep().await?;

        let mut cmd = tokio::process::Command::new(&self.config.rustc_path);
        cmd.arg(source_path).arg("-o").arg(binary_path).arg("--edition").arg("2021");

        // Link against serde_json.
        if let Some(dep_path) = &serde_json_dep {
            cmd.arg("--extern").arg(format!("serde_json={}", dep_path.display()));

            // Add the parent directory to the library search path so transitive
            // deps (serde, itoa, ryu, memchr, etc.) can be found.
            if let Some(parent) = dep_path.parent() {
                cmd.arg("-L").arg(format!("dependency={}", parent.display()));
            }
        }

        // Add any extra flags from config.
        for flag in &self.config.rustc_flags {
            cmd.arg(flag);
        }

        // Capture stdout and stderr.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let compile_timeout = request.sandbox.timeout;
        let compile_output = match tokio::time::timeout(compile_timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ExecutionError::CompileFailed(format!("failed to invoke rustc: {e}")));
            }
            Err(_) => {
                return Ok(Some(ExecutionResult {
                    status: ExecutionStatus::Timeout,
                    stdout: String::new(),
                    stderr: "compilation timed out".to_string(),
                    output: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    duration_ms: compile_timeout.as_millis() as u64,
                    metadata: None,
                }));
            }
        };

        if !compile_output.status.success() {
            let stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
            let (stderr, stderr_truncated) =
                truncate_output(stderr, request.sandbox.max_stderr_bytes);

            debug!(exit_code = compile_output.status.code(), "compilation failed");

            return Ok(Some(ExecutionResult {
                status: ExecutionStatus::CompileFailed,
                stdout: String::new(),
                stderr,
                output: None,
                exit_code: compile_output.status.code(),
                stdout_truncated: false,
                stderr_truncated,
                duration_ms: 0, // Will be set by caller if needed.
                metadata: None,
            }));
        }

        Ok(None)
    }

    /// Run the compiled binary with timeout enforcement and output capture.
    async fn run_binary(
        &self,
        binary_path: &std::path::Path,
        request: &ExecutionRequest,
        start: Instant,
    ) -> Result<ExecutionResult, ExecutionError> {
        let mut cmd = tokio::process::Command::new(binary_path);

        // Pass argv to the binary.
        for arg in &request.argv {
            cmd.arg(arg);
        }

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Kill the child when the handle is dropped (important for timeout).
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutionError::ExecutionFailed(format!("failed to spawn binary: {e}")))?;

        // Write structured input as JSON to stdin, then close it.
        if let Some(ref input) = request.input {
            if let Some(mut stdin) = child.stdin.take() {
                let json_bytes = serde_json::to_vec(input).unwrap_or_default();
                let _ = stdin.write_all(&json_bytes).await;
                drop(stdin);
            }
        } else if let Some(ref raw_stdin) = request.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(raw_stdin).await;
                drop(stdin);
            }
        } else {
            // Close stdin immediately so the child doesn't block reading.
            drop(child.stdin.take());
        }

        // Wait with timeout. `wait_with_output` consumes `child`, so on
        // timeout we rely on `kill_on_drop` to clean up the process.
        let output =
            match tokio::time::timeout(request.sandbox.timeout, child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    return Err(ExecutionError::ExecutionFailed(format!(
                        "failed to wait for binary: {e}"
                    )));
                }
                Err(_) => {
                    // Timeout — `kill_on_drop` will clean up the child process.
                    warn!("execution timed out");
                    let duration_ms = start.elapsed().as_millis() as u64;
                    return Ok(ExecutionResult {
                        status: ExecutionStatus::Timeout,
                        stdout: String::new(),
                        stderr: String::new(),
                        output: None,
                        exit_code: None,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        duration_ms,
                        metadata: None,
                    });
                }
            };

        let duration_ms = start.elapsed().as_millis() as u64;

        let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let raw_stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let (stdout, stdout_truncated) =
            truncate_output(raw_stdout, request.sandbox.max_stdout_bytes);
        let (stderr, stderr_truncated) =
            truncate_output(raw_stderr, request.sandbox.max_stderr_bytes);

        // Try to parse the last line of stdout as structured JSON output.
        // The harness prints the JSON output as the last line.
        let (structured_output, display_stdout) = extract_structured_output(&stdout);

        let status = if output.status.success() {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::Failed
        };

        debug!(
            exit_code = output.status.code(),
            duration_ms,
            has_structured_output = structured_output.is_some(),
            "execution completed"
        );

        Ok(ExecutionResult {
            status,
            stdout: display_stdout,
            stderr,
            output: structured_output,
            exit_code: output.status.code(),
            stdout_truncated,
            stderr_truncated,
            duration_ms,
            metadata: None,
        })
    }

    /// Locate the `serde_json` rlib for linking.
    ///
    /// If `serde_json_path` is configured, use that. Otherwise, try to find it
    /// by querying cargo for the serde_json package location.
    async fn find_serde_json_dep(&self) -> Result<Option<PathBuf>, ExecutionError> {
        if let Some(ref path) = self.config.serde_json_path {
            if path.exists() {
                return Ok(Some(path.clone()));
            }
            return Err(ExecutionError::ExecutionFailed(format!(
                "configured serde_json path does not exist: {}",
                path.display()
            )));
        }

        // Try to find serde_json rlib in the cargo target directory.
        // We look for it in the workspace's target/debug/deps directory.
        let output = tokio::process::Command::new("cargo")
            .args(["metadata", "--format-version=1", "--no-deps"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        if let Ok(output) = output {
            if output.status.success() {
                if let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                    if let Some(target_dir) = metadata["target_directory"].as_str() {
                        let deps_dir = PathBuf::from(target_dir).join("debug").join("deps");
                        if let Some(rlib) = find_rlib_in_dir(&deps_dir, "serde_json").await {
                            return Ok(Some(rlib));
                        }
                    }
                }
            }
        }

        // Fallback: return None and let rustc try to find it on its own.
        // This will likely fail, but the compile error will be descriptive.
        Ok(None)
    }
}

/// Find an rlib file matching the given crate name in a directory.
///
/// Picks the most recently modified matching file to avoid issues with stale
/// artifacts in the cargo deps directory.
async fn find_rlib_in_dir(dir: &std::path::Path, crate_name: &str) -> Option<PathBuf> {
    let prefix = format!("lib{crate_name}-");
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return None,
    };

    let mut rlibs = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && name_str.ends_with(".rlib") {
            if let Ok(metadata) = entry.metadata().await {
                if let Ok(modified) = metadata.modified() {
                    rlibs.push((entry.path(), modified));
                }
            }
        }
    }

    // Sort by modification time descending (latest first).
    rlibs.sort_by_key(|&(_, modified)| std::cmp::Reverse(modified));

    rlibs.into_iter().next().map(|(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_honest() {
        let executor = RustSandboxExecutor::default();
        let caps = executor.capabilities();
        assert_eq!(caps.isolation, ExecutionIsolation::HostLocal);
        assert!(caps.enforce_timeout);
        assert!(caps.supports_structured_output);
        assert!(!caps.enforce_network_policy);
        assert!(!caps.enforce_filesystem_policy);
        assert!(!caps.enforce_environment_policy);
    }

    #[test]
    fn supports_only_rust() {
        let executor = RustSandboxExecutor::default();
        assert!(executor.supports_language(&ExecutionLanguage::Rust));
        assert!(!executor.supports_language(&ExecutionLanguage::JavaScript));
        assert!(!executor.supports_language(&ExecutionLanguage::Python));
        assert!(!executor.supports_language(&ExecutionLanguage::Wasm));
        assert!(!executor.supports_language(&ExecutionLanguage::Command));
    }

    #[test]
    fn default_config() {
        let config = RustSandboxConfig::default();
        assert_eq!(config.rustc_path, "rustc");
        assert!(config.rustc_flags.is_empty());
        assert!(config.serde_json_path.is_none());
    }
}
