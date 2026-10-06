//! [`ProcessBackend`] — subprocess-based code execution via `tokio::process::Command`.
//!
//! This backend spawns child processes to execute code in various languages.
//! It enforces timeout and environment isolation, reads output under a byte cap
//! while the process runs, and kills the execution's process group on timeout
//! and on exit. On its own it does **not** enforce memory limits, network
//! isolation, or filesystem isolation; attach an OS enforcer with
//! [`ProcessBackend::with_sandbox`] for those.
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
use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{Span, instrument};

use crate::backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
use crate::child_io::{collect_output, note_truncation, truncate_utf8};
use crate::error::SandboxError;
use crate::sandbox::{AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy};
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
const WINDOWS_TOOLCHAIN_ENV_KEYS: &[&str] = &[
    "PATH",
    "LIB",
    "LIBPATH",
    "INCLUDE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

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
    /// Each execution runs in a fresh scratch directory holding its source file
    /// and compiler output. That directory becomes the working directory and is
    /// granted read-write on top of the policy, since the enforcers deny every
    /// path the policy does not name.
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
                // Linux bubblewrap builds a filesystem namespace and the macOS Seatbelt
                // profile is deny-by-default, so both confine reads to the policy's paths
                // plus the system runtime. The Windows enforcer is not implemented.
                filesystem_read_isolation: has_enforcer
                    && cfg!(any(target_os = "linux", target_os = "macos")),
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
        let toolchain_env = Self::toolchain_env();
        #[cfg(windows)]
        let has_msvc_library_path =
            toolchain_env.iter().any(|(key, _)| key.eq_ignore_ascii_case("LIB"));

        let compile_result = {
            let mut cmd = Command::new(&self.config.rustc_path);
            // Cargo applies the workspace's rust-lld setting, but this direct rustc
            // invocation does not read .cargo/config.toml. Hosted Windows runners put
            // Git's GNU `link.exe` ahead of MSVC on PATH, so naming rust-lld prevents
            // rustc from launching the unrelated Unix utility.
            #[cfg(windows)]
            cmd.arg("-Clinker=rust-lld");
            cmd.arg(&src_path).arg("-o").arg(&bin_path);
            self.run_command_with_env(cmd, request, &toolchain_env, Some(dir.path())).await?
        };

        #[cfg(windows)]
        let compile_result = {
            let mut result = compile_result;
            if result.exit_code != 0 && !has_msvc_library_path {
                result.stderr.push_str(
                    "\nWindows Rust linking requires the MSVC Build Tools and Windows SDK. \
                     Install the `Desktop development with C++` workload; ProcessBackend could \
                     not discover its LIB paths from this host.",
                );
            }
            result
        };

        if compile_result.exit_code != 0 {
            Span::current().record("exit_code", compile_result.exit_code);
            Span::current().record("duration_ms", compile_result.duration.as_millis() as u64);
            return Ok(compile_result);
        }

        // Run the compiled binary
        self.run_command(Command::new(&bin_path), request, Some(dir.path())).await
    }

    /// Executes Python code: write to temp file → run with python3.
    async fn execute_python(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let dir = tempfile::tempdir()?;
        let src_path = dir.path().join("script.py");
        std::fs::write(&src_path, &request.code)?;

        let mut cmd = Command::new(&self.config.python_path);
        cmd.arg(&src_path);
        self.run_command(cmd, request, Some(dir.path())).await
    }

    /// Executes JavaScript code: write to temp file → run with node.
    async fn execute_javascript(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        let dir = tempfile::tempdir()?;
        let src_path = dir.path().join("script.js");
        std::fs::write(&src_path, &request.code)?;

        let mut cmd = Command::new(&self.config.node_path);
        cmd.arg(&src_path);
        self.run_command(cmd, request, Some(dir.path())).await
    }

    /// Executes a raw shell command via the platform shell.
    async fn execute_command(&self, request: &ExecRequest) -> Result<ExecResult, SandboxError> {
        #[cfg(windows)]
        let cmd = {
            use std::os::windows::process::CommandExt;

            let mut c = Command::new("cmd");
            c.arg("/D").arg("/C");
            // `cmd.exe` does not follow CommandLineToArgvW escaping. In particular,
            // Command::arg turns embedded quotes into `\"`, which makes a quoted
            // executable path part of the program name. The command is already an
            // explicitly requested shell program, so pass it with cmd's own syntax.
            c.as_std_mut().raw_arg(&request.code);
            c
        };
        #[cfg(not(windows))]
        let cmd = {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&request.code);
            c
        };
        self.run_command(cmd, request, None).await
    }

    /// Shared execution logic: env isolation, stdin piping, timeout, output capture.
    ///
    /// When a [`SandboxEnforcer`] is configured, the command is wrapped with
    /// platform-specific sandbox restrictions before spawning.
    async fn run_command(
        &self,
        cmd: Command,
        request: &ExecRequest,
        scratch_dir: Option<&Path>,
    ) -> Result<ExecResult, SandboxError> {
        self.run_command_with_env(cmd, request, &[], scratch_dir).await
    }

    /// Variables a compiler needs to find its own tools.
    ///
    /// `rustc` shells out to a platform linker and resolves it through the environment.
    /// The MSVC linker also reads `LIB` to find the Windows and C runtime libraries.
    /// With the environment cleared it cannot link at all, so compilation gets a small
    /// platform-specific allowlist. On Windows, missing values are discovered from the
    /// installed Visual Studio Build Tools and Windows SDK.
    ///
    /// This widens what the compile phase can see compared with the run phase. An OS
    /// enforcer is what constrains it; see [`ProcessBackend::isolation`].
    fn toolchain_env() -> Vec<(String, OsString)> {
        // RUSTUP_TOOLCHAIN matters as much as RUSTUP_HOME: `rustc` on PATH is usually a rustup
        // shim, and without it the shim ignores the caller's selection and resolves
        // `rust-toolchain.toml` instead. That either compiles with a different toolchain than the
        // caller intended, or — when the pinned one is not installed — tries to download it and
        // fails against the sandbox's network denial, reporting "syncing channel updates" from
        // what looks like a compile error.
        let keys =
            if cfg!(windows) { WINDOWS_TOOLCHAIN_ENV_KEYS } else { NON_WINDOWS_TOOLCHAIN_ENV_KEYS };

        let environment: Vec<(String, OsString)> = keys
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|value| ((*key).to_string(), value)))
            .collect();

        #[cfg(windows)]
        let mut environment = environment;

        #[cfg(windows)]
        if !environment.iter().any(|(key, _)| key.eq_ignore_ascii_case("LIB"))
            && let Some(linker) = find_msvc_tools::find(std::env::consts::ARCH, "link.exe")
        {
            for (key, value) in linker.get_envs() {
                let Some(value) = value else {
                    continue;
                };
                let Some(allowed_key) = WINDOWS_TOOLCHAIN_ENV_KEYS
                    .iter()
                    .find(|allowed| key.eq_ignore_ascii_case(OsStr::new(allowed)))
                else {
                    continue;
                };
                if !environment
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(allowed_key))
                {
                    environment.push(((*allowed_key).to_string(), value.to_os_string()));
                }
            }
        }

        environment
    }

    /// Shared execution logic, with `extra_env` applied below policy and request values.
    ///
    /// `scratch_dir` is the execution's own temporary directory (source file, compiler
    /// output). Under an enforcer the sandboxed process is granted read-write access to it
    /// and starts in it; executions that bring none get a fresh one.
    async fn run_command_with_env(
        &self,
        cmd: Command,
        request: &ExecRequest,
        extra_env: &[(String, OsString)],
        scratch_dir: Option<&Path>,
    ) -> Result<ExecResult, SandboxError> {
        // Keeps a scratch directory created here alive until the execution ends.
        let mut owned_scratch: Option<tempfile::TempDir> = None;

        // Resolve a bare program name against the caller's PATH *before* clearing the
        // environment. Clearing first leaves the child with no PATH, and program
        // resolution then fails with ENOENT — so a backend configured with `"rustc"`,
        // `"python3"`, or `"node"` could not execute anything at all.
        let mut cmd = if let (Some(enforcer), Some(policy)) = (&self.enforcer, &self.policy) {
            let scratch = match scratch_dir {
                Some(dir) => dir.to_path_buf(),
                None => owned_scratch.insert(tempfile::tempdir()?).path().to_path_buf(),
            };
            // The enforcers deny every path the policy does not name, and the execution's
            // own files live in the scratch directory. It is created per execution and holds
            // nothing from the host, so granting it widens nothing.
            let mut policy = policy.clone();
            policy
                .allowed_paths
                .push(AllowedPath { path: scratch.clone(), mode: AccessMode::ReadWrite });

            let std_cmd = cmd.as_std();
            let args: Vec<OsString> = std_cmd.get_args().map(OsStr::to_owned).collect();
            let wrapped = enforcer.wrap_command(std_cmd.get_program(), &args, &policy)?;

            // Resolved before `configure_command`, which may attach spawn-time state (the
            // Linux seccomp descriptor, Windows process attributes) that rebuilding the
            // command afterwards would discard.
            let program = resolve_program(&wrapped.program)
                .map_or(wrapped.program, std::path::PathBuf::into_os_string);
            let mut wrapped_cmd = Command::new(program);
            wrapped_cmd.args(&wrapped.args).current_dir(&scratch);
            enforcer.configure_command(&mut wrapped_cmd, &policy)?;
            wrapped_cmd
        } else {
            match resolve_program(cmd.as_std().get_program()) {
                Some(resolved) => {
                    let args: Vec<OsString> =
                        cmd.as_std().get_args().map(OsStr::to_owned).collect();
                    let mut resolved_cmd = Command::new(resolved);
                    resolved_cmd.args(&args);
                    resolved_cmd
                }
                None => cmd,
            }
        };

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
        // detach, so the group is killed on timeout and again on exit.
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
        let child = cmd.spawn()?;

        // Both pipes are read concurrently with the cap applied as the bytes arrive, and stdin
        // is written alongside them inside the timeout.
        let cap = self.config.max_output_bytes;
        let stdin = request.stdin.as_ref().map(|input| input.as_bytes().to_vec());
        let output = collect_output(child, stdin, request.timeout, cap).await;
        let duration = start.elapsed();

        match output {
            Ok(Some(captured)) => {
                let exit_code = captured.status.code().unwrap_or(-1);
                if captured.stdout_truncated || captured.stderr_truncated {
                    tracing::warn!(
                        max_output_bytes = cap,
                        stdout.truncated = captured.stdout_truncated,
                        stderr.truncated = captured.stderr_truncated,
                        "sandbox output exceeded the cap and was truncated"
                    );
                }
                let stdout =
                    note_truncation(truncate_utf8(captured.stdout, cap), captured.stdout_truncated);
                let stderr =
                    note_truncation(truncate_utf8(captured.stderr, cap), captured.stderr_truncated);

                Span::current().record("exit_code", exit_code);
                Span::current().record("duration_ms", duration.as_millis() as u64);

                Ok(ExecResult { stdout, stderr, exit_code, duration })
            }
            Ok(None) => {
                Span::current().record("duration_ms", duration.as_millis() as u64);
                Err(SandboxError::Timeout { timeout: request.timeout })
            }
            Err(e) => {
                Err(SandboxError::ExecutionFailed(format!("failed to wait for child process: {e}")))
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
    #[cfg(windows)]
    async fn test_command_supports_quoted_script_paths() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("quoted helper.cmd");
        std::fs::write(&script, "@echo quoted-path-ok\r\n").unwrap();

        let backend = ProcessBackend::default();
        let request = make_request(Language::Command, &format!("\"{}\"", script.display()));
        let result = backend.execute(request).await.unwrap();

        assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
        assert!(result.stdout.contains("quoted-path-ok"), "stdout: {}", result.stdout);
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

    /// A `nohup … &` descendant must not outlive an execution that exits normally.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_normal_exit_terminates_background_descendants() {
        let backend = ProcessBackend::default();
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("escaped-child");
        let escaped_marker = marker.to_string_lossy().replace('\'', "'\\''");
        let code = format!(
            "nohup sh -c 'sleep 1; touch \"$0\"' '{escaped_marker}' >/dev/null 2>&1 & echo done"
        );

        let result = backend.execute(make_request(Language::Command, &code)).await.unwrap();
        assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
        assert_eq!(result.stdout.trim(), "done");

        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert!(!marker.exists(), "a background descendant survived a normal exit");
    }

    /// Stdin larger than a pipe buffer reaches a child that writes output before reading.
    ///
    /// Writing all of stdin before the output readers started deadlocked here: the child
    /// blocked on a full stdout pipe, the backend on a full stdin pipe, outside the timeout.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_large_stdin_does_not_deadlock_against_output() {
        let backend = ProcessBackend::default();
        let mut request = make_request(Language::Command, "head -c 262144 /dev/zero; wc -c");
        request.stdin = Some("s".repeat(512 * 1_024));

        let result = tokio::time::timeout(Duration::from_secs(60), backend.execute(request))
            .await
            .expect("the execution deadlocked")
            .expect("the execution completes");

        assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
        assert!(
            result.stdout.trim_end().ends_with("524288"),
            "stdin was not fully delivered: {:?}",
            &result.stdout[result.stdout.len().saturating_sub(32)..]
        );
    }

    /// A child that never reads its stdin is still bounded by the request timeout.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_unread_stdin_is_bounded_by_the_timeout() {
        let backend = ProcessBackend::default();
        let mut request = make_request(Language::Command, "sleep 30");
        request.stdin = Some("s".repeat(4 * 1_024 * 1_024));
        request.timeout = Duration::from_millis(300);

        let started = std::time::Instant::now();
        let result = backend.execute(request).await;

        assert!(matches!(result, Err(SandboxError::Timeout { .. })), "got: {result:?}");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
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
                "LIBPATH",
                "INCLUDE",
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
