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
//! | Wasm       | Not supported — use `WasmBackend` instead            |
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

use std::ffi::{OsStr, OsString};
use std::time::Instant;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{Span, instrument};

use crate::backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
use crate::error::SandboxError;
use crate::sandbox::{SandboxEnforcer, SandboxPolicy};
use crate::types::{ExecRequest, ExecResult, Language};

/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_024 * 1_024;

/// Host variables exposed only while compiling Rust on non-Windows platforms.
const NON_WINDOWS_TOOLCHAIN_ENV_KEYS: &[&str] = &[
    "PATH",
    "DEVELOPER_DIR",
    "SDKROOT",
    "HOME",
    "TMPDIR",
    "RUSTUP_HOME",
    "CARGO_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// Host variables exposed only while compiling Rust with the MSVC toolchain.
///
/// `LIB` is the linker's library search path. The remaining Windows-specific
/// values support temporary files, system DLL discovery, and rustup's default
/// toolchain location without copying the full developer-shell environment.
const WINDOWS_TOOLCHAIN_ENV_KEYS: &[&str] =
    &["PATH", "LIB", "SystemRoot", "TEMP", "TMP", "USERPROFILE", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"];

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
    /// Maximum bytes retained from each of stdout and stderr. Default: 1 MiB.
    ///
    /// The limit is applied as the pipes are read, so it bounds memory rather than only the
    /// reported output. Excess is drained and discarded, and the returned text carries a
    /// truncation notice.
    pub max_output_bytes: usize,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            rustc_path: "rustc".to_string(),
            python_path: "python3".to_string(),
            node_path: "node".to_string(),
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

/// Subprocess-based sandbox backend.
///
/// Executes code by spawning child processes with `tokio::process::Command`.
/// Enforces timeout via `tokio::time::timeout` and environment isolation
/// via `env_clear()`. Optionally enforces filesystem and network isolation
/// when a [`SandboxEnforcer`] is configured via [`with_sandbox()`](Self::with_sandbox).
///
/// # Example
///
/// ```rust
/// use adk_sandbox::{ProcessBackend, SandboxBackend};
///
/// let backend = ProcessBackend::default();
/// assert_eq!(backend.name(), "process");
/// ```
///
/// # With OS-level sandbox
///
/// ```rust,ignore
/// use adk_sandbox::{ProcessBackend, ProcessConfig, SandboxPolicyBuilder, get_enforcer};
///
/// let enforcer = get_enforcer()?;
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr/lib")
///     .allow_read_write("/tmp/work")
///     .build();
///
/// let backend = ProcessBackend::with_sandbox(
///     ProcessConfig::default(),
///     enforcer,
///     policy,
/// );
/// assert!(backend.capabilities().enforced_limits.filesystem_write_isolation);
/// ```
pub struct ProcessBackend {
    config: ProcessConfig,
    enforcer: Option<Box<dyn SandboxEnforcer>>,
    policy: Option<SandboxPolicy>,
}

/// How much isolation a backend actually provides.
///
/// Reported so a caller can tell the two apart rather than assuming the stronger one
/// because the crate is named `adk-sandbox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationClass {
    /// A child process with a cleared environment, a timeout, and its own process group.
    ///
    /// The OS applies no further restriction: the code can read the host filesystem and
    /// reach the network. This is what [`ProcessBackend::default`] provides.
    SubprocessOnly,
    /// A child process wrapped by an OS enforcer — Seatbelt, bubblewrap, or AppContainer
    /// — under a [`SandboxPolicy`].
    OsEnforced,
}

/// Resolve a bare program name to an absolute path using the caller's `PATH`.
///
/// Returns `None` when the name already contains a path separator, or when nothing on
/// `PATH` matches — in which case the command is left as it was so the spawn error still
/// names the program the caller asked for.
fn resolve_program(program: &OsStr) -> Option<std::path::PathBuf> {
    let as_path = std::path::Path::new(program);
    if as_path.components().count() > 1 {
        return None;
    }

    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
}

impl ProcessBackend {
    /// Creates a new `ProcessBackend` with the given configuration.
    ///
    /// The result is [`IsolationClass::SubprocessOnly`] until an enforcer and policy are
    /// attached; see [`ProcessBackend::isolation`].
    pub fn new(config: ProcessConfig) -> Self {
        Self { config, enforcer: None, policy: None }
    }

    /// How much isolation this backend applies.
    ///
    /// Check this before treating execution as sandboxed. Without an enforcer *and* a
    /// policy, execution is subprocess isolation only.
    pub fn isolation(&self) -> IsolationClass {
        match (self.enforcer.is_some(), self.policy.is_some()) {
            (true, true) => IsolationClass::OsEnforced,
            _ => IsolationClass::SubprocessOnly,
        }
    }

    /// Creates a new `ProcessBackend` with OS-level sandbox enforcement.
    ///
    /// All executions through this backend will be sandboxed with the given
    /// policy. The enforcer wraps commands with platform-specific restrictions
    /// (Seatbelt on macOS, bubblewrap on Linux, AppContainer on Windows).
    ///
    /// If different tools need different policies, create multiple
    /// `ProcessBackend` instances.
    pub fn with_sandbox(
        config: ProcessConfig,
        enforcer: Box<dyn SandboxEnforcer>,
        policy: SandboxPolicy,
    ) -> Self {
        Self { config, enforcer: Some(enforcer), policy: Some(policy) }
    }
}

impl Default for ProcessBackend {
    fn default() -> Self {
        Self::new(ProcessConfig::default())
    }
}

// ProcessBackend can't derive Debug because Box<dyn SandboxEnforcer> doesn't impl Debug.
impl std::fmt::Debug for ProcessBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessBackend")
            .field("config", &self.config)
            .field("enforcer", &self.enforcer.as_ref().map(|e| e.name()))
            .field("policy", &self.policy)
            .finish()
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

/// Appends a truncation notice when output was discarded.
///
/// A model that receives silently-cut output has no way to know it is incomplete, so the notice
/// travels with the data rather than only appearing in a log. Mirrors the convention in
/// adk-python's `tools/environment` toolset.
fn note_truncation(mut text: String, discarded: bool) -> String {
    if discarded {
        text.push_str("\n... (truncated: output exceeded the configured limit)");
    }
    text
}

/// Reads `reader` to EOF, accumulating at most `cap` bytes.
///
/// Bytes past `cap` are read and discarded rather than left in the pipe. Stopping the read
/// would block the child on a full pipe buffer and stall it until the execution timeout, so
/// the drain continues even though the data is thrown away.
///
/// Returns the retained bytes and whether anything was discarded.
async fn read_capped<R>(mut reader: R, cap: usize) -> std::io::Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut retained = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut discarded = false;

    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let room = cap.saturating_sub(retained.len());
        if room == 0 {
            discarded = true;
            continue;
        }
        let take = room.min(read);
        retained.extend_from_slice(&chunk[..take]);
        if take < read {
            discarded = true;
        }
    }

    Ok((retained, discarded))
}

#[async_trait]
impl SandboxBackend for ProcessBackend {
    fn name(&self) -> &str {
        "process"
    }

    fn capabilities(&self) -> BackendCapabilities {
        let has_enforcer = self.enforcer.is_some();
        let denies_network = self.policy.as_ref().is_some_and(|p| !p.allow_network);

        BackendCapabilities {
            supported_languages: vec![
                Language::Rust,
                Language::Python,
                Language::JavaScript,
                Language::TypeScript,
                Language::Command,
            ],
            isolation_class: if has_enforcer {
                "process+sandbox".to_string()
            } else {
                "process".to_string()
            },
            enforced_limits: EnforcedLimits {
                timeout: true,
                memory: false,
                network_isolation: has_enforcer && denies_network,
                filesystem_write_isolation: has_enforcer,
                // The macOS profile denies writes, network, and fork but leaves reads
                // open, so read isolation is not claimed there. Linux bubblewrap builds
                // a filesystem namespace, which does confine reads.
                filesystem_read_isolation: has_enforcer && cfg!(target_os = "linux"),
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

        // Compile through the same path as execution. Building the command here and
        // calling `output()` directly skipped the enforcer, the timeout, and the
        // process group — and Rust compilation is not inert: `include_str!` and
        // procedural macros read files and can run arbitrary code at compile time, so
        // the compiler needs the same boundary as the binary it produces.
        let compile_result = {
            let mut cmd = Command::new(&self.config.rustc_path);
            cmd.arg(&src_path).arg("-o").arg(&bin_path);
            self.run_command_with_env(cmd, request, &Self::toolchain_env()).await?
        };

        if compile_result.exit_code != 0 {
            Span::current().record("exit_code", compile_result.exit_code);
            Span::current().record("duration_ms", compile_result.duration.as_millis() as u64);
            return Ok(compile_result);
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

    /// Executes a raw shell command via the platform shell.
    async fn execute_command(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&request.code);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&request.code);
            c
        };
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
    ///
    /// When a [`SandboxEnforcer`] is configured, the command is wrapped with
    /// platform-specific sandbox restrictions before spawning.
    async fn run_command(
        &self,
        cmd: Command,
        request: &ExecRequest,
    ) -> Result<ExecResult, SandboxError> {
        self.run_command_with_env(cmd, request, &[]).await
    }

    /// Variables a compiler needs to find its own tools.
    ///
    /// `rustc` shells out to a platform linker and resolves it through the environment.
    /// The MSVC linker also reads `LIB` to find the Windows and C runtime libraries.
    /// With the environment cleared it cannot link at all, so compilation gets a small
    /// platform-specific allowlist from the caller when those values are set.
    ///
    /// This widens what the compile phase can see compared with the run phase. An OS
    /// enforcer is what constrains it; see [`ProcessBackend::isolation`].
    fn toolchain_env() -> Vec<(String, String)> {
        // RUSTUP_TOOLCHAIN matters as much as RUSTUP_HOME: `rustc` on PATH is usually a rustup
        // shim, and without it the shim ignores the caller's selection and resolves
        // `rust-toolchain.toml` instead. That either compiles with a different toolchain than the
        // caller intended, or — when the pinned one is not installed — tries to download it and
        // fails against the sandbox's network denial, reporting "syncing channel updates" from
        // what looks like a compile error.
        let keys =
            if cfg!(windows) { WINDOWS_TOOLCHAIN_ENV_KEYS } else { NON_WINDOWS_TOOLCHAIN_ENV_KEYS };

        keys.iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).to_string(), value)))
            .collect()
    }

    /// Shared execution logic, with `extra_env` applied below policy and request values.
    async fn run_command_with_env(
        &self,
        cmd: Command,
        request: &ExecRequest,
        extra_env: &[(String, String)],
    ) -> Result<ExecResult, SandboxError> {
        // If a sandbox enforcer is configured, wrap the command.
        // We extract the program and args from the pre-built Command,
        // pass them through the enforcer, and create a new Command.
        let mut cmd = if let (Some(enforcer), Some(policy)) = (&self.enforcer, &self.policy) {
            let std_cmd = cmd.as_std();
            let program = std_cmd.get_program();
            let args: Vec<OsString> = std_cmd.get_args().map(OsStr::to_owned).collect();

            let wrapped = enforcer.wrap_command(program, &args, policy)?;

            let mut new_cmd = Command::new(&wrapped.program);
            new_cmd.args(&wrapped.args);

            // Apply any post-construction configuration (e.g., Windows AppContainer)
            enforcer.configure_command(&mut new_cmd, policy)?;

            new_cmd
        } else {
            cmd
        };

        // Resolve a bare program name against the caller's PATH *before* clearing the
        // environment. Clearing first leaves the child with no PATH, and program
        // resolution then fails with ENOENT — so a backend configured with `"rustc"`,
        // `"python3"`, or `"node"` could not execute anything at all.
        {
            let program = cmd.as_std().get_program().to_owned();
            if let Some(resolved) = resolve_program(&program) {
                let args: Vec<OsString> = cmd.as_std().get_args().map(OsStr::to_owned).collect();
                let mut resolved_cmd = Command::new(resolved);
                resolved_cmd.args(&args);
                cmd = resolved_cmd;
            }
        }

        // Environment precedence: the policy supplies defaults for every execution, and
        // the request overrides them per call. `SandboxPolicy::env` was previously
        // ignored entirely, so a policy that set variables silently supplied none.
        cmd.env_clear();
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if let Some(policy) = &self.policy {
            for (k, v) in &policy.env {
                cmd.env(k, v);
            }
        }
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        cmd.kill_on_drop(true);

        // Give each execution its own process group. `kill_on_drop` only
        // targets the immediate child, which is not enough for shell tools:
        // compilers, scripts, and background jobs can otherwise survive a
        // timeout. Descendants inherit this group unless they deliberately
        // detach, so the timeout path can terminate the execution tree.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if request.stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }

        let start = Instant::now();
        let mut child = cmd.spawn()?;
        #[cfg(unix)]
        let process_group = child.id().map(|id| id as i32);

        // Pipe stdin if provided
        if let Some(ref input) = request.stdin
            && let Some(mut stdin_handle) = child.stdin.take()
        {
            stdin_handle.write_all(input.as_bytes()).await?;
            drop(stdin_handle);
        }

        // Read both pipes concurrently with the cap applied as the bytes arrive. Buffering the
        // whole output first and truncating afterwards let a process allocate without bound
        // before the limit was consulted, so the cap did not limit memory at all.
        let cap = self.config.max_output_bytes;
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_reader = tokio::spawn(async move {
            match stdout_pipe {
                Some(pipe) => read_capped(pipe, cap).await,
                None => Ok((Vec::new(), false)),
            }
        });
        let stderr_reader = tokio::spawn(async move {
            match stderr_pipe {
                Some(pipe) => read_capped(pipe, cap).await,
                None => Ok((Vec::new(), false)),
            }
        });

        let output = tokio::time::timeout(request.timeout, async {
            let status = child.wait().await?;
            let (stdout, stdout_discarded) =
                stdout_reader.await.map_err(std::io::Error::other)??;
            let (stderr, stderr_discarded) =
                stderr_reader.await.map_err(std::io::Error::other)??;
            Ok::<_, std::io::Error>((status, stdout, stdout_discarded, stderr, stderr_discarded))
        })
        .await;
        let duration = start.elapsed();

        match output {
            Ok(Ok((status, stdout_bytes, stdout_discarded, stderr_bytes, stderr_discarded))) => {
                let exit_code = status.code().unwrap_or(-1);
                if stdout_discarded || stderr_discarded {
                    tracing::warn!(
                        max_output_bytes = cap,
                        stdout.truncated = stdout_discarded,
                        stderr.truncated = stderr_discarded,
                        "sandbox output exceeded the cap and was truncated"
                    );
                }
                let cap = self.config.max_output_bytes;
                let stdout = note_truncation(truncate_utf8(stdout_bytes, cap), stdout_discarded);
                let stderr = note_truncation(truncate_utf8(stderr_bytes, cap), stderr_discarded);

                Span::current().record("exit_code", exit_code);
                Span::current().record("duration_ms", duration.as_millis() as u64);

                Ok(ExecResult { stdout, stderr, exit_code, duration })
            }
            Ok(Err(e)) => {
                Err(SandboxError::ExecutionFailed(format!("failed to wait for child process: {e}")))
            }
            Err(_) => {
                // Timeout — terminate the Unix process group before
                // `kill_on_drop` cleans up the immediate child. This prevents
                // background descendants from escaping the execution limit.
                #[cfg(unix)]
                if let Some(group) = process_group {
                    // SAFETY: `group` is the positive PID returned for the
                    // child we just placed in a new process group. A negative
                    // PID asks kill(2) to signal that process group only.
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                    }
                }
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
        // Windows processes need SYSTEMROOT for DLL loading and basic operation.
        if let Ok(sr) = std::env::var("SYSTEMROOT") {
            env.insert("SYSTEMROOT".to_string(), sr);
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
        // Skip if node is not available (e.g. minimal CI images)
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping test_javascript_execution: node not found");
            return;
        }
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
        let code =
            if cfg!(windows) { "ping -n 11 127.0.0.1".to_string() } else { "sleep 10".to_string() };
        let mut request = make_request(Language::Command, &code);
        request.timeout = Duration::from_secs(1);
        let result = backend.execute(request).await;
        assert!(
            matches!(result, Err(SandboxError::Timeout { .. })),
            "expected Timeout, got: {result:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_timeout_terminates_background_descendants() {
        let backend = ProcessBackend::default();
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("escaped-child");
        let escaped_marker = marker.to_string_lossy().replace('\'', "'\\''");
        let code = format!("(sleep 1; touch '{escaped_marker}') & wait");
        let mut request = make_request(Language::Command, &code);
        request.timeout = Duration::from_millis(100);

        let result = backend.execute(request).await;
        assert!(matches!(result, Err(SandboxError::Timeout { .. })));
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(!marker.exists(), "a background descendant survived the execution timeout");
    }

    #[tokio::test]
    #[cfg(not(windows))]
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
    #[cfg(windows)]
    async fn test_environment_isolation() {
        let backend = ProcessBackend::default();
        let mut env = HashMap::new();
        env.insert("MY_TEST_VAR".to_string(), "test_value".to_string());
        let request = ExecRequest {
            language: Language::Command,
            code: "set MY_TEST_VAR".to_string(),
            stdin: None,
            timeout: Duration::from_secs(10),
            memory_limit_mb: None,
            env,
        };
        let result = backend.execute(request).await.unwrap();
        assert!(result.stdout.contains("MY_TEST_VAR=test_value"), "stdout: {}", result.stdout);
    }

    #[tokio::test]
    async fn test_nonzero_exit_code() {
        let backend = ProcessBackend::default();
        let code = if cfg!(windows) { "exit /b 42" } else { "exit 42" };
        let request = make_request(Language::Command, code);
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

    /// `read_capped` must retain at most `cap` bytes regardless of how much arrives.
    ///
    /// This is the property the streaming read exists for, and it is not observable from
    /// `ExecResult`: `truncate_utf8` caps the *reported* string either way, so an end-to-end
    /// test passes even when the whole stream was buffered first. Asserting on the retained
    /// buffer is what distinguishes bounded memory from a bounded report.
    #[tokio::test]
    async fn read_capped_retains_at_most_the_cap() {
        let cap = 4_096;
        // 256x the cap, so a buffering implementation would allocate 1 MiB here.
        let source = vec![b'x'; cap * 256];

        let (retained, discarded) = read_capped(&source[..], cap).await.expect("reads");

        assert_eq!(retained.len(), cap, "retained buffer must stop at the cap");
        assert!(discarded, "the overflow must be reported as discarded");
    }

    /// Everything is retained when the stream is smaller than the cap, and nothing is flagged.
    #[tokio::test]
    async fn read_capped_retains_everything_under_the_cap() {
        let source = vec![b'y'; 100];

        let (retained, discarded) = read_capped(&source[..], 4_096).await.expect("reads");

        assert_eq!(retained, source);
        assert!(!discarded);
    }

    /// A stream landing exactly on the cap is not reported as truncated.
    #[tokio::test]
    async fn read_capped_handles_the_exact_boundary() {
        let cap = 8_192;
        let source = vec![b'z'; cap];

        let (retained, discarded) = read_capped(&source[..], cap).await.expect("reads");

        assert_eq!(retained.len(), cap);
        assert!(!discarded, "reaching the cap exactly discards nothing");
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
        assert!(!caps.enforced_limits.filesystem_write_isolation);
        assert!(!caps.enforced_limits.filesystem_read_isolation);
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

    #[test]
    fn windows_compiler_environment_is_a_minimal_allowlist() {
        assert_eq!(
            WINDOWS_TOOLCHAIN_ENV_KEYS,
            &[
                "PATH",
                "LIB",
                "SystemRoot",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "RUSTUP_HOME",
                "RUSTUP_TOOLCHAIN",
            ]
        );
    }
}
