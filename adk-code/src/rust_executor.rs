//! Rust-specific executor with a check → build → execute pipeline.
//!
//! [`RustExecutor`] compiles Rust code through a two-step pipeline (check + build)
//! and delegates execution to a configured [`SandboxBackend`] from `adk-sandbox`.
//! It wraps user code in the same harness template as the legacy
//! [`RustSandboxExecutor`](crate::RustSandboxExecutor).
//!
//! # Pipeline
//!
//! 1. **Check**: `rustc --edition 2021 --error-format=json` → parse diagnostics → halt on errors
//! 2. **Build**: `rustc --edition 2021 -o binary` → compile to binary using harness template
//! 3. **Execute**: delegate to [`SandboxBackend`] with [`Language::Command`] and the binary path
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_code::{RustExecutor, RustExecutorConfig};
//! use adk_sandbox::{ProcessBackend, SandboxBackend};
//! use std::sync::Arc;
//!
//! let backend: Arc<dyn SandboxBackend> = Arc::new(ProcessBackend::default());
//! let executor = RustExecutor::new(backend, RustExecutorConfig::default());
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_sandbox::{ExecRequest, ExecResult, Language, SandboxBackend};
use tracing::{debug, info, instrument};

use crate::diagnostics::{RustDiagnostic, parse_diagnostics};
use crate::error::CodeError;
use crate::harness::{HARNESS_TEMPLATE, extract_structured_output, validate_rust_source};

/// Configuration for the [`RustExecutor`] pipeline.
///
/// # Example
///
/// ```rust
/// use adk_code::RustExecutorConfig;
///
/// let config = RustExecutorConfig::default();
/// assert_eq!(config.rustc_path, "rustc");
/// ```
#[derive(Debug, Clone)]
pub struct RustExecutorConfig {
    /// Path to the `rustc` compiler binary.
    pub rustc_path: String,
    /// Explicit path to the `serde_json` rlib. If `None`, the executor
    /// attempts automatic discovery via `cargo metadata`.
    pub serde_json_path: Option<PathBuf>,
    /// Extra flags passed to `rustc` during compilation.
    pub rustc_flags: Vec<String>,
}

impl Default for RustExecutorConfig {
    fn default() -> Self {
        Self { rustc_path: "rustc".to_string(), serde_json_path: None, rustc_flags: vec![] }
    }
}

/// Result of a successful [`RustExecutor::execute`] call.
///
/// Contains the sandbox execution result plus any compiler diagnostics
/// (warnings) that were emitted during the check step.
#[derive(Debug, Clone)]
pub struct CodeResult {
    /// The sandbox execution result (stdout, stderr, exit_code, duration).
    pub exec_result: ExecResult,
    /// Compiler diagnostics from the check step (warnings only — errors halt execution).
    pub diagnostics: Vec<RustDiagnostic>,
    /// Structured JSON output extracted from the last stdout line, if any.
    pub output: Option<serde_json::Value>,
    /// Display stdout (everything before the structured output line).
    pub display_stdout: String,
}

/// Rust-specific executor that compiles code through a check → build → execute pipeline
/// and delegates execution to a [`SandboxBackend`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_code::{RustExecutor, RustExecutorConfig};
/// use adk_sandbox::ProcessBackend;
/// use std::sync::Arc;
///
/// let backend = Arc::new(ProcessBackend::default());
/// let executor = RustExecutor::new(backend, RustExecutorConfig::default());
/// let result = executor.execute("fn run(input: serde_json::Value) -> serde_json::Value { input }", None).await?;
/// ```
pub struct RustExecutor {
    backend: Arc<dyn SandboxBackend>,
    config: RustExecutorConfig,
}

impl RustExecutor {
    /// Create a new executor with the given sandbox backend and configuration.
    pub fn new(backend: Arc<dyn SandboxBackend>, config: RustExecutorConfig) -> Self {
        Self { backend, config }
    }

    /// Execute Rust code through the check → build → execute pipeline.
    ///
    /// The `input` parameter is optional JSON that will be serialized to stdin
    /// for the harness `run()` function.
    ///
    /// # Errors
    ///
    /// - [`CodeError::InvalidCode`] if the source fails pre-compilation validation
    /// - [`CodeError::CompileError`] if the check step finds error-level diagnostics
    /// - [`CodeError::DependencyNotFound`] if `serde_json` cannot be located
    /// - [`CodeError::Sandbox`] if the backend fails during execution
    #[instrument(skip_all, fields(backend = %self.backend.name()))]
    pub async fn execute(
        &self,
        code: &str,
        input: Option<&serde_json::Value>,
        timeout: Duration,
    ) -> Result<CodeResult, CodeError> {
        // Pre-compilation validation.
        validate_rust_source(code).map_err(|e| CodeError::InvalidCode(e.to_string()))?;

        // Create temp directory for compilation artifacts.
        let tmp_dir = tempfile::tempdir()
            .map_err(|e| CodeError::InvalidCode(format!("failed to create temp directory: {e}")))?;

        let source_path = tmp_dir.path().join("main.rs");
        let binary_path = tmp_dir.path().join("main");

        // Write harnessed source.
        let harnessed_source = HARNESS_TEMPLATE.replace("{user_code}", code);
        tokio::fs::write(&source_path, &harnessed_source)
            .await
            .map_err(|e| CodeError::InvalidCode(format!("failed to write source file: {e}")))?;

        debug!(source_path = %source_path.display(), "wrote harnessed source");

        // Locate serde_json dependency.
        let serde_json_path = self.find_serde_json_dep().await?;

        // Step 1: Check — compile with --error-format=json, parse diagnostics.
        let diagnostics = self.check(&source_path, &serde_json_path, timeout).await?;

        // Step 2: Build — compile to binary.
        self.build(&source_path, &binary_path, &serde_json_path, timeout).await?;

        info!("compilation succeeded, delegating execution to sandbox backend");

        // Step 3: Execute — delegate to SandboxBackend.
        let stdin = input.map(|v| serde_json::to_string(v).unwrap_or_default());
        let exec_request = ExecRequest {
            language: Language::Command,
            code: binary_path.to_string_lossy().to_string(),
            stdin,
            timeout,
            memory_limit_mb: None,
            env: HashMap::new(),
        };

        let exec_result = self.backend.execute(exec_request).await?;

        // Extract structured output from stdout.
        let (output, display_stdout) = extract_structured_output(&exec_result.stdout);

        debug!(
            exit_code = exec_result.exit_code,
            duration_ms = exec_result.duration.as_millis() as u64,
            has_output = output.is_some(),
            "execution completed"
        );

        Ok(CodeResult { exec_result, diagnostics, output, display_stdout })
    }

    /// Check step: compile with `--error-format=json` and `--emit=metadata`
    /// to parse diagnostics without producing a binary.
    ///
    /// Returns warnings (errors halt with [`CodeError::CompileError`]).
    async fn check(
        &self,
        source_path: &Path,
        serde_json_path: &Option<PathBuf>,
        timeout: Duration,
    ) -> Result<Vec<RustDiagnostic>, CodeError> {
        // Use a temp directory for metadata output to avoid /dev/null issues on macOS.
        let check_dir = tempfile::tempdir().map_err(|e| {
            CodeError::InvalidCode(format!("failed to create check temp directory: {e}"))
        })?;
        let metadata_out = check_dir.path().join("check_output");

        let mut cmd = tokio::process::Command::new(&self.config.rustc_path);
        cmd.arg(source_path)
            .arg("--edition")
            .arg("2021")
            .arg("--error-format=json")
            .arg("--color")
            .arg("never")
            .arg("--emit=metadata")
            .arg("-o")
            .arg(&metadata_out);

        self.add_serde_json_flags(&mut cmd, serde_json_path);
        self.add_extra_flags(&mut cmd);

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(CodeError::InvalidCode(format!(
                    "failed to invoke rustc for check: {e}"
                )));
            }
            Err(_) => {
                return Err(CodeError::InvalidCode("check step timed out".to_string()));
            }
        };

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let diagnostics = parse_diagnostics(&stderr);

        let has_errors = diagnostics.iter().any(|d| d.level == "error");
        if has_errors {
            debug!(
                error_count = diagnostics.iter().filter(|d| d.level == "error").count(),
                "check step found errors"
            );
            return Err(CodeError::CompileError { diagnostics, stderr });
        }

        let warning_count = diagnostics.iter().filter(|d| d.level == "warning").count();
        if warning_count > 0 {
            debug!(warning_count, "check step found warnings");
        }

        Ok(diagnostics)
    }

    /// Build step: compile to a binary.
    async fn build(
        &self,
        source_path: &Path,
        binary_path: &Path,
        serde_json_path: &Option<PathBuf>,
        timeout: Duration,
    ) -> Result<(), CodeError> {
        let mut cmd = tokio::process::Command::new(&self.config.rustc_path);
        cmd.arg(source_path).arg("-o").arg(binary_path).arg("--edition").arg("2021");

        self.add_serde_json_flags(&mut cmd, serde_json_path);
        self.add_extra_flags(&mut cmd);

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(CodeError::InvalidCode(format!(
                    "failed to invoke rustc for build: {e}"
                )));
            }
            Err(_) => {
                return Err(CodeError::InvalidCode("build step timed out".to_string()));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let diagnostics = parse_diagnostics(&stderr);
            return Err(CodeError::CompileError { diagnostics, stderr });
        }

        Ok(())
    }

    /// Add `--extern serde_json=...` and `-L dependency=...` flags to a rustc command.
    fn add_serde_json_flags(
        &self,
        cmd: &mut tokio::process::Command,
        serde_json_path: &Option<PathBuf>,
    ) {
        if let Some(dep_path) = serde_json_path {
            cmd.arg("--extern").arg(format!("serde_json={}", dep_path.display()));

            if let Some(parent) = dep_path.parent() {
                cmd.arg("-L").arg(format!("dependency={}", parent.display()));
            }
        }
    }

    /// Add extra rustc flags from config.
    fn add_extra_flags(&self, cmd: &mut tokio::process::Command) {
        for flag in &self.config.rustc_flags {
            cmd.arg(flag);
        }
    }

    /// Locate the `serde_json` rlib using the fallback chain:
    /// 1. Explicit config path → use if exists
    /// 2. `cargo metadata` → find rlib in target/debug/deps
    /// 3. Return [`CodeError::DependencyNotFound`] with instructions
    async fn find_serde_json_dep(&self) -> Result<Option<PathBuf>, CodeError> {
        let mut searched = Vec::new();

        // 1. Explicit config path.
        if let Some(ref path) = self.config.serde_json_path {
            if path.exists() {
                debug!(path = %path.display(), "using configured serde_json path");
                return Ok(Some(path.clone()));
            }
            searched.push(format!("config: {}", path.display()));
        }

        // 2. cargo metadata scan.
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
                        searched.push(format!("cargo metadata: {}", deps_dir.display()));
                        if let Some(rlib) = find_rlib_in_dir(&deps_dir, "serde_json").await {
                            debug!(path = %rlib.display(), "found serde_json via cargo metadata");
                            return Ok(Some(rlib));
                        }
                    }
                }
            }
        } else {
            searched.push("cargo metadata: command failed".to_string());
        }

        // 3. Descriptive error.
        Err(CodeError::DependencyNotFound { name: "serde_json".to_string(), searched })
    }
}

/// Find an rlib file matching the given crate name in a directory.
async fn find_rlib_in_dir(dir: &Path, crate_name: &str) -> Option<PathBuf> {
    let prefix = format!("lib{crate_name}-");
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return None,
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && name_str.ends_with(".rlib") {
            return Some(entry.path());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_sandbox::{BackendCapabilities, EnforcedLimits, SandboxError};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A mock sandbox backend for unit testing.
    struct MockBackend {
        /// Captured requests for assertion.
        captured: Mutex<Vec<ExecRequest>>,
        /// Canned response to return.
        response: Mutex<Option<Result<ExecResult, SandboxError>>>,
    }

    impl MockBackend {
        fn new(response: Result<ExecResult, SandboxError>) -> Self {
            Self { captured: Mutex::new(Vec::new()), response: Mutex::new(Some(response)) }
        }

        fn success(stdout: &str) -> Self {
            Self::new(Ok(ExecResult {
                stdout: stdout.to_string(),
                stderr: String::new(),
                exit_code: 0,
                duration: Duration::from_millis(10),
            }))
        }
    }

    #[async_trait]
    impl SandboxBackend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supported_languages: vec![Language::Command],
                isolation_class: "mock".to_string(),
                enforced_limits: EnforcedLimits {
                    timeout: true,
                    memory: false,
                    network_isolation: false,
                    filesystem_isolation: false,
                    environment_isolation: false,
                },
            }
        }

        async fn execute(&self, request: ExecRequest) -> Result<ExecResult, SandboxError> {
            self.captured.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(SandboxError::ExecutionFailed("no canned response".to_string())))
        }
    }

    #[test]
    fn default_config() {
        let config = RustExecutorConfig::default();
        assert_eq!(config.rustc_path, "rustc");
        assert!(config.serde_json_path.is_none());
        assert!(config.rustc_flags.is_empty());
    }

    #[tokio::test]
    async fn check_valid_code_produces_no_errors() {
        let backend = Arc::new(MockBackend::success(r#"{"result":42}"#));
        let executor = RustExecutor::new(backend, RustExecutorConfig::default());

        let tmp_dir = tempfile::tempdir().unwrap();
        let source_path = tmp_dir.path().join("valid.rs");
        let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    input
}
"#;
        let harnessed = HARNESS_TEMPLATE.replace("{user_code}", code);
        tokio::fs::write(&source_path, &harnessed).await.unwrap();

        // The check step may fail if serde_json is not available, but it should
        // NOT fail with CodeError::InvalidCode for valid source.
        let result = executor.check(&source_path, &None, Duration::from_secs(30)).await;

        // If serde_json is not found, rustc will produce an error about the
        // missing extern crate — that's a compile error, not invalid code.
        match result {
            Ok(diagnostics) => {
                // No error-level diagnostics.
                assert!(
                    !diagnostics.iter().any(|d| d.level == "error"),
                    "expected no error diagnostics for valid code"
                );
            }
            Err(CodeError::CompileError { diagnostics, .. }) => {
                // Acceptable: serde_json not linked, so rustc reports an error.
                assert!(
                    diagnostics.iter().any(|d| d.level == "error"),
                    "expected error diagnostics when serde_json is missing"
                );
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn check_invalid_code_returns_compile_error() {
        let backend = Arc::new(MockBackend::success(""));
        let executor = RustExecutor::new(backend, RustExecutorConfig::default());

        let tmp_dir = tempfile::tempdir().unwrap();
        let source_path = tmp_dir.path().join("invalid.rs");
        // Deliberately broken Rust code.
        let code = "fn broken( { }";
        tokio::fs::write(&source_path, code).await.unwrap();

        let result = executor.check(&source_path, &None, Duration::from_secs(30)).await;

        match result {
            Err(CodeError::CompileError { diagnostics, stderr }) => {
                assert!(
                    diagnostics.iter().any(|d| d.level == "error"),
                    "expected at least one error diagnostic"
                );
                assert!(!stderr.is_empty(), "expected non-empty stderr");
            }
            other => panic!("expected CompileError, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_warnings_are_returned_without_halting() {
        let backend = Arc::new(MockBackend::success(""));
        let executor = RustExecutor::new(backend, RustExecutorConfig::default());

        let tmp_dir = tempfile::tempdir().unwrap();
        let source_path = tmp_dir.path().join("warnings.rs");
        // Code with an unused variable warning.
        let code = "fn main() { let x = 42; }";
        tokio::fs::write(&source_path, code).await.unwrap();

        let result = executor.check(&source_path, &None, Duration::from_secs(30)).await;

        match result {
            Ok(diagnostics) => {
                // Should have at least one warning about unused variable.
                assert!(
                    diagnostics.iter().any(|d| d.level == "warning"),
                    "expected at least one warning diagnostic, got: {diagnostics:?}"
                );
            }
            Err(CodeError::CompileError { diagnostics, .. }) => {
                // If there are errors too, that's unexpected for this code.
                panic!("unexpected compile errors: {diagnostics:?}");
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn serde_json_discovery_config_path_exists() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let fake_rlib = tmp_dir.path().join("libserde_json-abc123.rlib");
        tokio::fs::write(&fake_rlib, b"fake rlib").await.unwrap();

        let config =
            RustExecutorConfig { serde_json_path: Some(fake_rlib.clone()), ..Default::default() };
        let backend = Arc::new(MockBackend::success(""));
        let executor = RustExecutor::new(backend, config);

        let result = executor.find_serde_json_dep().await.unwrap();
        assert_eq!(result, Some(fake_rlib));
    }

    #[tokio::test]
    async fn serde_json_discovery_config_path_missing() {
        let config = RustExecutorConfig {
            serde_json_path: Some(PathBuf::from("/nonexistent/libserde_json.rlib")),
            ..Default::default()
        };
        let backend = Arc::new(MockBackend::success(""));
        let executor = RustExecutor::new(backend, config);

        // With a non-existent config path and no cargo metadata fallback,
        // this should try cargo metadata and then fail with DependencyNotFound.
        let result = executor.find_serde_json_dep().await;
        match result {
            Err(CodeError::DependencyNotFound { name, searched }) => {
                assert_eq!(name, "serde_json");
                assert!(
                    searched.iter().any(|s| s.contains("/nonexistent/")),
                    "expected searched to include the config path, got: {searched:?}"
                );
            }
            // If cargo metadata happens to find it, that's also acceptable.
            Ok(Some(_)) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn code_error_from_sandbox_error() {
        let sandbox_err = SandboxError::Timeout { timeout: Duration::from_secs(5) };
        let code_err: CodeError = sandbox_err.into();
        assert!(matches!(code_err, CodeError::Sandbox(_)));
        assert!(code_err.to_string().contains("sandbox error"));
    }

    #[test]
    fn validate_rejects_fn_main() {
        let code = "fn main() { }";
        let result = validate_rust_source(code);
        assert!(result.is_err());
    }

    #[test]
    fn validate_accepts_valid_run() {
        let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    input
}
"#;
        assert!(validate_rust_source(code).is_ok());
    }
}
