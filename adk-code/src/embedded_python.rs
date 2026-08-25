//! Embedded Python executors backed by the Pydantic Monty interpreter.
//!
//! [`MontyExecutorBuilder`] configures OS-access grants, host functions, and
//! resource limits once, then produces either executor product:
//!
//! - [`build_one_shot`](MontyExecutorBuilder::build_one_shot) →
//!   [`MontyOneShotExecutor`] — a fresh interpreter per
//!   [`execute`](crate::CodeExecutor::execute) call, no state persists;
//! - [`build_repl`](MontyExecutorBuilder::build_repl) →
//!   [`MontyReplExecutor`] — interpreter state (variables, function
//!   definitions, imports) persists across calls.
//!
//! # Security Model
//!
//! Isolation combines **explicit policy** with **enforcement by omission**:
//!
//! - **Explicit policy.** Every OS call Monty can emit — filesystem
//!   reads/writes, `os.getenv`/`os.environ`, `date.today()`/`datetime.now()` —
//!   is serviced in place against the filesystem roots, environment variables,
//!   and clock the host grants at construction. Ungranted access raises a
//!   catchable Python `OSError` in-script.
//! - **Enforcement by omission.** Monty has no network or subprocess surface
//!   at all, so those remain impossible regardless of configuration.
//! - **Grants vs. request policy.** The builder's grants are the maximum
//!   access any script can have; the per-request
//!   [`SandboxPolicy`] may only *narrow* within them. A grant covers its
//!   entire directory subtree, so a request for a granted mount or any
//!   subdirectory of one succeeds. A request exceeding the grants is rejected
//!   fail-closed with [`ExecutionError::UnsupportedPolicy`] naming the excess
//!   path or variable, before any code runs. `FilesystemPolicy::None` /
//!   `EnvironmentPolicy::None` simply grant nothing for that call.
//! - **Boundary enforcement is delegated.** The mount boundary
//!   (canonicalization + symlink-escape detection) is enforced by the
//!   `monty-fs` crate, pinned to an exact `0.0.x` release in `Cargo.toml`.
//!   Bumping that pin is a security-relevant change and warrants re-review of
//!   its path-resolution behavior.
//! - **Host functions run as host code.** Registered [`HostFunction`]s are the
//!   *user's own* trust boundary, not Monty's — the interpreter sandbox does
//!   not contain their side effects.
//! - **Timeouts.** `SandboxPolicy::timeout` bounds *interpreter* time via
//!   Monty's `ResourceLimits::max_duration` (real preemption). Host-function
//!   execution gets its own wall-clock bound
//!   ([`host_function_timeout`](MontyExecutorBuilder::host_function_timeout)).
//! - **REPL memory accounting.** The resource tracker is serialized with the
//!   session, so `max_memory` bounds the *cumulative* session heap, not
//!   per-call allocation. The per-call time budget is reset on every call.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_code::{MontyExecutorBuilder, PathAccess};
//! use serde_json::json;
//!
//! # fn main() -> Result<(), adk_code::MontyBuildError> {
//! let builder = MontyExecutorBuilder::new()
//!     .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
//!     .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
//!     .environ_var("PROJECT", "acme")
//!     .system_clock()
//!     .function_fn("row_count", "Count rows in the loaded dataset.", |args, _kwargs| async move {
//!         Ok(json!(args.len()))
//!     })
//!     .max_memory(64 * 1024 * 1024);
//!
//! let one_shot = builder.clone().build_one_shot()?;
//! let repl = builder.build_repl()?;
//! # Ok(())
//! # }
//! ```

mod convert;
mod drive;
mod host_fn;
mod os_access;
mod prompt;

// The Monty crates, re-exported so every consumer of this module's public
// surface (which speaks `MontyObject`, `OsFunctionCall`, `MountTable`, ...)
// names the exact release this crate was built against — the version is
// stated once, here, and cannot drift between crates.
pub use {monty, monty_fs, monty_types};

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use monty_types::ResourceLimits;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::debug;

use crate::{
    BackendCapabilities, CodeExecutor, EnvironmentPolicy, ExecutionError, ExecutionIsolation,
    ExecutionLanguage, ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus,
    FilesystemPolicy, SandboxPolicy, validate_request,
};

use drive::{CappedStdout, DriveEnd, PausedCall, ReplSegment, RunSegment, Tracker};
use host_fn::FunctionRegistry;
use os_access::OsAccess;
use prompt::ModeWording;

pub use convert::{json_to_monty, monty_to_json};
pub use host_fn::{HostFunction, HostFunctionError, MontyBuildError};
pub use os_access::{PathAccess, resolve_os_call};
pub use prompt::SUPPORTED_PATH_METHODS;

/// Default wall-clock bound for a single host-function call.
const DEFAULT_HOST_FUNCTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Default cumulative memory cap. LLM-generated Python can contain a runaway
/// allocation; this keeps a run bounded. Override with
/// [`MontyExecutorBuilder::max_memory`].
const DEFAULT_MAX_MEMORY: usize = 256 * 1024 * 1024;

/// Configures OS-access grants, host functions, and resource limits for the
/// Monty executors, then produces either product with a terminal build method.
///
/// The builder is cheaply cloneable (grants are small, the registry holds
/// `Arc<dyn HostFunction>`), so one configuration can mint many executors.
///
/// Defaults are fully sandboxed: no filesystem mounts, an empty environment,
/// and no clock.
///
/// # Example
///
/// ```rust
/// use adk_code::MontyExecutorBuilder;
///
/// # fn main() -> Result<(), adk_code::MontyBuildError> {
/// let one_shot = MontyExecutorBuilder::new().build_one_shot()?;
/// let repl = MontyExecutorBuilder::new().build_repl()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct MontyExecutorBuilder {
    os: OsAccess,
    functions: Vec<Arc<dyn HostFunction>>,
    max_memory: Option<usize>,
    host_function_timeout: Duration,
    script_name: String,
}

impl fmt::Debug for MontyExecutorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MontyExecutorBuilder")
            .field("os", &self.os)
            .field("functions", &self.functions.iter().map(|hf| hf.name()).collect::<Vec<_>>())
            .field("max_memory", &self.max_memory)
            .field("host_function_timeout", &self.host_function_timeout)
            .field("script_name", &self.script_name)
            .finish()
    }
}

impl Default for MontyExecutorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MontyExecutorBuilder {
    /// Start with the fully sandboxed defaults: no filesystem mounts, an empty
    /// environment map, and no clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            os: OsAccess::default(),
            functions: Vec::new(),
            max_memory: Some(DEFAULT_MAX_MEMORY),
            host_function_timeout: DEFAULT_HOST_FUNCTION_TIMEOUT,
            script_name: "python_snippet".to_string(),
        }
    }

    /// Make a host directory available to scripts at `virtual_path`.
    ///
    /// `virtual_path` is the absolute path a script uses (e.g. `/data`);
    /// `host_path` is the real directory it maps to. The mount boundary is
    /// enforced by Monty (canonicalization + symlink-escape detection) — a
    /// script can never escape it. Call repeatedly to expose several
    /// directories.
    ///
    /// Virtual paths are validated at `build_*()`: each must be a normalized
    /// absolute path (no trailing slash, no `.` or `..` components) unique
    /// across mounts.
    #[must_use]
    pub fn allow_path(
        mut self,
        virtual_path: impl Into<String>,
        host_path: impl Into<PathBuf>,
        access: PathAccess,
    ) -> Self {
        self.os.mounts.push(os_access::MountSpec {
            virtual_path: virtual_path.into(),
            host_path: host_path.into(),
            access,
        });
        self
    }

    /// Replace the environment map exposed via `os.getenv` / `os.environ`.
    ///
    /// Only the entries provided here are visible to scripts; the host process
    /// environment is never exposed implicitly.
    #[must_use]
    pub fn environ<K, V>(mut self, vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.os.environ = vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        self
    }

    /// Add or overwrite a single environment variable.
    #[must_use]
    pub fn environ_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.os.environ.insert(key.into(), value.into());
        self
    }

    /// Grant host-clock access: `date.today()` and `datetime.now()` read the
    /// real system time. Without this grant they raise a catchable `OSError`.
    #[must_use]
    pub fn system_clock(mut self) -> Self {
        self.os.system_clock = true;
        self
    }

    /// Register a host function callable from Python by bare name.
    ///
    /// Registered names are validated at `build_*()`: they must be valid
    /// Python identifiers, unique, and must not collide with built-ins.
    #[must_use]
    pub fn function(mut self, function: Arc<dyn HostFunction>) -> Self {
        self.functions.push(function);
        self
    }

    /// Register a closure as a host function — the common case.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::MontyExecutorBuilder;
    /// use serde_json::json;
    ///
    /// # fn main() -> Result<(), adk_code::MontyBuildError> {
    /// let executor = MontyExecutorBuilder::new()
    ///     .function_fn("double", "Double a number.", |args, _kwargs| async move {
    ///         let n = args.first().and_then(|v| v.as_i64()).unwrap_or(0);
    ///         Ok(json!(n * 2))
    ///     })
    ///     .build_one_shot()?;
    /// # let _ = executor;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn function_fn<F, Fut>(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        func: F,
    ) -> Self
    where
        F: Fn(Vec<Value>, Map<String, Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, HostFunctionError>> + Send + 'static,
    {
        self.function(Arc::new(host_fn::ClosureHostFunction::new(name, description, func)))
    }

    /// Cap the interpreter's memory use in bytes (default 256 MiB).
    ///
    /// For a REPL executor this bounds the cumulative session heap. Exceeding
    /// it raises Python's `MemoryError` (the result reports `Failed`).
    #[must_use]
    pub fn max_memory(mut self, bytes: usize) -> Self {
        self.max_memory = Some(bytes);
        self
    }

    /// Wall-clock bound for a single host-function call (default 30 s), so a
    /// hung user function cannot wedge `execute()`. On expiry the script
    /// receives a catchable exception.
    #[must_use]
    pub fn host_function_timeout(mut self, timeout: Duration) -> Self {
        self.host_function_timeout = timeout;
        self
    }

    /// The script name used in tracebacks (default `"python_snippet"`).
    #[must_use]
    pub fn script_name(mut self, name: impl Into<String>) -> Self {
        self.script_name = name.into();
        self
    }

    /// Build the one-shot product: a fresh interpreter per `execute()` call.
    ///
    /// # Errors
    ///
    /// Returns a [`MontyBuildError`] when the host-function registry is
    /// invalid (invalid identifier, duplicate name, built-in collision,
    /// reserved name) or a mount's virtual path is not a normalized,
    /// unique absolute path.
    pub fn build_one_shot(self) -> Result<MontyOneShotExecutor, MontyBuildError> {
        Ok(MontyOneShotExecutor { core: self.build_core(ModeWording::OneShot)? })
    }

    /// Build the REPL product: interpreter state persists across `execute()`
    /// calls.
    ///
    /// # Errors
    ///
    /// Returns a [`MontyBuildError`] when the host-function registry is
    /// invalid (invalid identifier, duplicate name, built-in collision,
    /// reserved name) or a mount's virtual path is not a normalized,
    /// unique absolute path.
    pub fn build_repl(self) -> Result<MontyReplExecutor, MontyBuildError> {
        Ok(MontyReplExecutor {
            core: self.build_core(ModeWording::Repl)?,
            repl_state: Mutex::new(None),
        })
    }

    fn build_core(self, mode: ModeWording) -> Result<Arc<MontyCore>, MontyBuildError> {
        os_access::validate_mounts(&self.os.mounts)?;
        let registry = FunctionRegistry::build(self.functions)?;
        let prompt_snippet = prompt::render_prompt_snippet(&self.os, &registry, mode);
        let mut base_limits = ResourceLimits::default();
        if let Some(bytes) = self.max_memory {
            base_limits = base_limits.max_memory(bytes);
        }
        Ok(Arc::new(MontyCore {
            grants: self.os,
            registry,
            base_limits,
            script_name: self.script_name,
            host_function_timeout: self.host_function_timeout,
            prompt_snippet,
        }))
    }
}

/// The immutable configuration shared by both executor products: grants,
/// host-function registry, resource limits, and the cached prompt snippet.
#[derive(Debug)]
struct MontyCore {
    grants: OsAccess,
    registry: FunctionRegistry,
    /// Memory (and other non-time) limits; `max_duration` is applied
    /// per-request from `SandboxPolicy::timeout`.
    base_limits: ResourceLimits,
    script_name: String,
    host_function_timeout: Duration,
    prompt_snippet: String,
}

impl MontyCore {
    /// A fresh tracker for one call, with the request's time budget.
    fn tracker(&self, timeout: Duration) -> Tracker {
        Tracker::new(self.base_limits.clone().max_duration(timeout))
    }

    /// A [`SandboxPolicy`] requesting exactly this executor's grants —
    /// what a caller passes to use everything the executor offers.
    fn granted_policy(&self) -> SandboxPolicy {
        let mut policy = SandboxPolicy::strict_python();
        if !self.grants.mounts.is_empty() {
            let mut read_only = Vec::new();
            let mut read_write = Vec::new();
            for mount in &self.grants.mounts {
                let path = PathBuf::from(&mount.virtual_path);
                match mount.access {
                    PathAccess::ReadOnly => read_only.push(path),
                    PathAccess::ReadWrite => read_write.push(path),
                }
            }
            policy.filesystem = FilesystemPolicy::Paths { read_only, read_write };
        }
        if !self.grants.environ.is_empty() {
            policy.environment =
                EnvironmentPolicy::AllowList(self.grants.environ.keys().cloned().collect());
        }
        policy
    }

    /// Await a registered host function with the configured wall-clock bound.
    /// The `Err` message becomes a catchable in-script exception.
    async fn call_host_function(
        &self,
        name: &str,
        args: Vec<Value>,
        kwargs: Map<String, Value>,
    ) -> Result<Value, String> {
        // The drive loop only pauses for registered names, so the lookup
        // cannot fail in practice; fail soft if it somehow does.
        let Some(function) = self.registry.get(name) else {
            return Err(self.registry.unknown_function_message(name));
        };
        debug!(host_fn.name = %name, args.count = args.len(), "calling host function");
        match tokio::time::timeout(self.host_function_timeout, function.call(args, kwargs)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err(format!(
                "host function '{name}' timed out after {:?}",
                self.host_function_timeout
            )),
        }
    }
}

/// Shared capability report; only the persistence flags differ by mode.
fn monty_capabilities(persistent: bool) -> BackendCapabilities {
    BackendCapabilities {
        isolation: ExecutionIsolation::InProcess,
        // Enforced by explicit policy (granted mounts/environ/clock) and by
        // omission (Monty has no network or subprocess surface at all).
        enforce_network_policy: true,
        enforce_filesystem_policy: true,
        enforce_environment_policy: true,
        // ResourceLimits::max_duration — real preemption inside the VM.
        enforce_timeout: true,
        // The final expression value becomes `ExecutionResult::output`.
        supports_structured_output: true,
        supports_process_execution: false,
        supports_persistent_workspace: persistent,
        supports_interactive_sessions: persistent,
    }
}

/// Extract non-empty Python source from a request payload.
fn source_code(request: &ExecutionRequest) -> Result<String, ExecutionError> {
    match &request.payload {
        ExecutionPayload::Source { code } => {
            if code.trim().is_empty() {
                Err(ExecutionError::InvalidRequest("empty Python source".to_string()))
            } else {
                Ok(code.clone())
            }
        }
        ExecutionPayload::GuestModule { .. } => Err(ExecutionError::InvalidRequest(
            "Monty executors do not support guest modules".to_string(),
        )),
    }
}

/// Truncate `s` to at most `max` bytes on a char boundary; returns whether
/// anything was cut.
fn truncate_utf8(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    true
}

/// Assemble the [`ExecutionResult`] from a finished drive. Script-level
/// failures are data (`Failed`/`Timeout` status), never `Err`.
fn build_result(
    end: DriveEnd,
    stdout: CappedStdout,
    policy: &SandboxPolicy,
    started: Instant,
) -> ExecutionResult {
    // The collector already enforces the cap during the drive; the post-hoc
    // truncation is belt-and-braces and should never fire.
    let (mut stdout, capped) = stdout.into_parts();
    let stdout_truncated = truncate_utf8(&mut stdout, policy.max_stdout_bytes) || capped;
    let (status, mut stderr) = match end.error {
        None => (ExecutionStatus::Success, String::new()),
        Some(rendered) if end.timed_out => (ExecutionStatus::Timeout, rendered),
        Some(rendered) => (ExecutionStatus::Failed, rendered),
    };
    let stderr_truncated = truncate_utf8(&mut stderr, policy.max_stderr_bytes);
    ExecutionResult {
        status,
        stdout,
        stderr,
        output: end.value,
        exit_code: None,
        stdout_truncated,
        stderr_truncated,
        duration_ms: started.elapsed().as_millis() as u64,
        metadata: None,
    }
}

fn join_error(err: tokio::task::JoinError) -> ExecutionError {
    ExecutionError::InternalError(format!("interpreter thread panicked: {err}"))
}

/// One-shot Monty executor: a fresh interpreter per
/// [`execute`](CodeExecutor::execute) call.
///
/// Build with [`MontyExecutorBuilder::build_one_shot`]. Lifecycle methods keep
/// the no-op trait defaults — every call is independent. See the
/// [module docs](self) for the security model.
///
/// # Example
///
/// ```rust,no_run
/// use adk_code::{
///     CodeExecutor, ExecutionLanguage, ExecutionPayload, ExecutionRequest,
///     MontyExecutorBuilder, SandboxPolicy,
/// };
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let executor = MontyExecutorBuilder::new().build_one_shot()?;
/// let request = ExecutionRequest {
///     language: ExecutionLanguage::Python,
///     payload: ExecutionPayload::Source { code: "1 + 1".to_string() },
///     argv: vec![],
///     stdin: None,
///     input: None,
///     sandbox: SandboxPolicy::strict_python(),
///     identity: None,
/// };
/// let result = executor.execute(request).await?;
/// assert_eq!(result.output, Some(serde_json::json!(2)));
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct MontyOneShotExecutor {
    core: Arc<MontyCore>,
}

impl MontyOneShotExecutor {
    /// A [`SandboxPolicy`] requesting exactly this executor's grants.
    ///
    /// The per-request policy may only narrow within the grants, so
    /// [`SandboxPolicy::strict_python`] (no filesystem, no environment) runs
    /// fully sandboxed even on a granting executor. Use this policy to run
    /// with everything the executor offers.
    #[must_use]
    pub fn granted_policy(&self) -> SandboxPolicy {
        self.core.granted_policy()
    }
}

#[async_trait]
impl CodeExecutor for MontyOneShotExecutor {
    fn name(&self) -> &str {
        "monty-one-shot"
    }

    fn capabilities(&self) -> BackendCapabilities {
        monty_capabilities(false)
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        *lang == ExecutionLanguage::Python
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some(self.core.prompt_snippet.clone())
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let started = Instant::now();
        validate_request(&self.capabilities(), &[ExecutionLanguage::Python], &request)?;
        let code = source_code(&request)?;
        let os = self.core.grants.narrowed(&request.sandbox)?;
        let timeout = request.sandbox.timeout;

        // First segment: parse, start, drive to the first pause or the end.
        let core = &self.core;
        let tracker = core.tracker(timeout);
        let (mut segment, mut stdout) = {
            let os = os.clone();
            let registry = core.registry.clone();
            let script_name = core.script_name.clone();
            let input = request.input.clone();
            let mut stdout = CappedStdout::new(request.sandbox.max_stdout_bytes);
            tokio::task::spawn_blocking(move || {
                let segment = drive::start_run(
                    &code,
                    &script_name,
                    input,
                    tracker,
                    &os,
                    &registry,
                    &mut stdout,
                )?;
                Ok::<_, ExecutionError>((segment, stdout))
            })
            .await
            .map_err(join_error)??
        };

        // Segment boundaries: await the host function in async context, then
        // resume the serialized progress on a fresh blocking segment.
        loop {
            match segment {
                RunSegment::Finished(end) => {
                    return Ok(build_result(end, stdout, &request.sandbox, started));
                }
                RunSegment::Paused(PausedCall { name, args, kwargs, progress_bytes }) => {
                    let outcome = core.call_host_function(&name, args, kwargs).await;
                    let os = os.clone();
                    let registry = core.registry.clone();
                    (segment, stdout) = tokio::task::spawn_blocking(move || {
                        let mut stdout = stdout;
                        let segment = drive::resume_run(
                            &progress_bytes,
                            outcome,
                            &os,
                            &registry,
                            &mut stdout,
                        )?;
                        Ok::<_, ExecutionError>((segment, stdout))
                    })
                    .await
                    .map_err(join_error)??;
                }
            }
        }
    }
}

/// One REPL session at rest: the postcard-serialized `MontyRepl` plus the
/// effective OS policy captured on the session's first call.
struct ReplSession {
    bytes: Vec<u8>,
    /// `None` until the first `execute()` establishes the session policy.
    policy: Option<OsAccess>,
}

/// REPL Monty executor: interpreter state (variables, function definitions,
/// imports) persists across [`execute`](CodeExecutor::execute) calls.
///
/// Build with [`MontyExecutorBuilder::build_repl`]. The executor stores the
/// **serialized** interpreter between calls (Monty's types are not designed to
/// be held across `await` points), guarded by a `Mutex` so snippets apply in
/// submission order. Monty preserves the session through Python-level
/// exceptions, so a failed snippet does not destroy accumulated state.
///
/// # Lifecycle
///
/// [`start`](CodeExecutor::start) initializes an empty interpreter session,
/// [`stop`](CodeExecutor::stop) drops it, and
/// [`restart`](CodeExecutor::restart) resets it. Calling `execute()` before
/// `start()` lazily initializes the session.
///
/// # Policy consistency
///
/// A session's effective OS policy must not vary between calls (a mount
/// visible in call 1 must not silently vanish in call 2 while state persists).
/// The first call's effective policy is captured; a later call whose policy
/// differs is rejected with guidance to `restart()`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_code::{
///     CodeExecutor, ExecutionLanguage, ExecutionPayload, ExecutionRequest,
///     MontyExecutorBuilder, SandboxPolicy,
/// };
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let executor = MontyExecutorBuilder::new().build_repl()?;
/// let request = |code: &str| ExecutionRequest {
///     language: ExecutionLanguage::Python,
///     payload: ExecutionPayload::Source { code: code.to_string() },
///     argv: vec![],
///     stdin: None,
///     input: None,
///     sandbox: SandboxPolicy::strict_python(),
///     identity: None,
/// };
/// executor.execute(request("x = 41")).await?;
/// let result = executor.execute(request("x + 1")).await?;
/// assert_eq!(result.output, Some(serde_json::json!(42)));
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct MontyReplExecutor {
    core: Arc<MontyCore>,
    /// The serialized `MontyRepl` state. `None` = no session.
    repl_state: Mutex<Option<ReplSession>>,
}

impl fmt::Debug for ReplSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplSession")
            .field("bytes.len", &self.bytes.len())
            .field("policy", &self.policy)
            .finish()
    }
}

impl MontyReplExecutor {
    /// A [`SandboxPolicy`] requesting exactly this executor's grants.
    ///
    /// See [`MontyOneShotExecutor::granted_policy`].
    #[must_use]
    pub fn granted_policy(&self) -> SandboxPolicy {
        self.core.granted_policy()
    }
}

#[async_trait]
impl CodeExecutor for MontyReplExecutor {
    fn name(&self) -> &str {
        "monty-repl"
    }

    fn capabilities(&self) -> BackendCapabilities {
        monty_capabilities(true)
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        *lang == ExecutionLanguage::Python
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some(self.core.prompt_snippet.clone())
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let started = Instant::now();
        validate_request(&self.capabilities(), &[ExecutionLanguage::Python], &request)?;
        let code = source_code(&request)?;
        let os = self.core.grants.narrowed(&request.sandbox)?;
        let timeout = request.sandbox.timeout;

        // The lock serializes REPL calls so snippets apply in submission
        // order, and guards the state cell across the whole call.
        let mut guard = self.repl_state.lock().await;
        if let Some(session) = guard.as_ref()
            && let Some(policy) = session.policy.as_ref()
            && *policy != os
        {
            return Err(ExecutionError::InvalidRequest(
                "the request's effective OS policy differs from this REPL session's established \
                 policy; a session's mounts and environment must not vary between calls. \
                 Call restart() to start a fresh session under the new policy."
                    .to_string(),
            ));
        }
        let prior_bytes = guard.as_ref().map(|session| session.bytes.clone());

        // First segment: load (or create) the session, feed the snippet,
        // drive to the first pause or the end.
        let core = &self.core;
        let tracker = core.tracker(timeout);
        let (mut segment, mut stdout) = {
            let os = os.clone();
            let registry = core.registry.clone();
            let script_name = core.script_name.clone();
            let input = request.input.clone();
            let mut stdout = CappedStdout::new(request.sandbox.max_stdout_bytes);
            tokio::task::spawn_blocking(move || {
                let segment = drive::feed_repl(
                    prior_bytes.as_deref(),
                    &script_name,
                    tracker,
                    timeout,
                    &code,
                    input,
                    &os,
                    &registry,
                    &mut stdout,
                )?;
                Ok::<_, ExecutionError>((segment, stdout))
            })
            .await
            .map_err(join_error)??
        };

        loop {
            match segment {
                ReplSegment::Finished { end, repl_bytes } => {
                    *guard = Some(ReplSession { bytes: repl_bytes, policy: Some(os) });
                    return Ok(build_result(end, stdout, &request.sandbox, started));
                }
                ReplSegment::Paused(PausedCall { name, args, kwargs, progress_bytes }) => {
                    let outcome = core.call_host_function(&name, args, kwargs).await;
                    let os = os.clone();
                    let registry = core.registry.clone();
                    (segment, stdout) = tokio::task::spawn_blocking(move || {
                        let mut stdout = stdout;
                        let segment = drive::resume_repl(
                            &progress_bytes,
                            outcome,
                            &os,
                            &registry,
                            &mut stdout,
                        )?;
                        Ok::<_, ExecutionError>((segment, stdout))
                    })
                    .await
                    .map_err(join_error)??;
                }
            }
        }
    }

    /// Initialize an empty interpreter session. A no-op if one exists.
    async fn start(&self) -> Result<(), ExecutionError> {
        let mut guard = self.repl_state.lock().await;
        if guard.is_none() {
            // The time budget is installed per call from `SandboxPolicy::timeout`.
            let tracker = Tracker::new(self.core.base_limits.clone());
            let bytes = drive::fresh_repl_bytes(&self.core.script_name, tracker)?;
            *guard = Some(ReplSession { bytes, policy: None });
        }
        Ok(())
    }

    /// Drop the interpreter session (variables, functions, imports).
    async fn stop(&self) -> Result<(), ExecutionError> {
        *self.repl_state.lock().await = None;
        Ok(())
    }

    async fn is_running(&self) -> bool {
        self.repl_state.lock().await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capabilities_encode_the_mode() {
        let one_shot = MontyExecutorBuilder::new().build_one_shot().unwrap();
        let caps = one_shot.capabilities();
        assert!(!caps.supports_interactive_sessions);
        assert!(!caps.supports_persistent_workspace);

        let repl = MontyExecutorBuilder::new().build_repl().unwrap();
        let caps = repl.capabilities();
        assert!(caps.supports_interactive_sessions);
        assert!(caps.supports_persistent_workspace);
    }

    #[test]
    fn one_cloned_builder_yields_both_products_with_identical_grants() {
        let builder = MontyExecutorBuilder::new()
            .allow_path("/data", "/srv/data", PathAccess::ReadOnly)
            .environ_var("PROJECT", "acme")
            .system_clock()
            .function_fn("noop", "Do nothing.", |_args, _kwargs| async move { Ok(json!(null)) });

        let one_shot = builder.clone().build_one_shot().unwrap();
        let repl = builder.build_repl().unwrap();

        assert_eq!(one_shot.core.grants, repl.core.grants);
        assert_eq!(
            one_shot.core.registry.names().collect::<Vec<_>>(),
            repl.core.registry.names().collect::<Vec<_>>()
        );
        // Only the mode wording differs between the cached snippets.
        assert_ne!(one_shot.core.prompt_snippet, repl.core.prompt_snippet);
    }

    #[test]
    fn build_rejects_invalid_registries() {
        let err = MontyExecutorBuilder::new()
            .function_fn(
                "len",
                "Shadow a builtin.",
                |_args, _kwargs| async move { Ok(json!(null)) },
            )
            .build_one_shot()
            .unwrap_err();
        assert_eq!(err, MontyBuildError::BuiltinCollision("len".to_string()));

        let err = MontyExecutorBuilder::new()
            .function_fn("dup", "One.", |_args, _kwargs| async move { Ok(json!(null)) })
            .function_fn("dup", "Two.", |_args, _kwargs| async move { Ok(json!(null)) })
            .build_repl()
            .unwrap_err();
        assert_eq!(err, MontyBuildError::DuplicateFunctionName("dup".to_string()));
    }

    #[test]
    fn build_rejects_invalid_mount_paths() {
        for bad in ["data", "/", "/data/", "/data//sub", "/data/../out"] {
            let err = MontyExecutorBuilder::new()
                .allow_path(bad, "/srv/data", PathAccess::ReadOnly)
                .build_one_shot()
                .unwrap_err();
            assert!(
                matches!(err, MontyBuildError::InvalidMountPath { .. }),
                "expected InvalidMountPath for {bad:?}, got {err:?}"
            );
        }
        let err = MontyExecutorBuilder::new()
            .allow_path("/data", "/srv/a", PathAccess::ReadOnly)
            .allow_path("/data", "/srv/b", PathAccess::ReadWrite)
            .build_repl()
            .unwrap_err();
        assert_eq!(err, MontyBuildError::DuplicateMountPath("/data".to_string()));
    }

    #[test]
    fn granted_policy_mirrors_the_grants() {
        let one_shot = MontyExecutorBuilder::new()
            .allow_path("/data", "/srv/data", PathAccess::ReadOnly)
            .allow_path("/out", "/srv/out", PathAccess::ReadWrite)
            .environ_var("PROJECT", "acme")
            .build_one_shot()
            .unwrap();
        let policy = one_shot.granted_policy();
        assert_eq!(
            policy.filesystem,
            FilesystemPolicy::Paths {
                read_only: vec![PathBuf::from("/data")],
                read_write: vec![PathBuf::from("/out")],
            }
        );
        assert_eq!(policy.environment, EnvironmentPolicy::AllowList(vec!["PROJECT".to_string()]));

        let sandboxed = MontyExecutorBuilder::new().build_one_shot().unwrap();
        assert_eq!(sandboxed.granted_policy().filesystem, FilesystemPolicy::None);
        assert_eq!(sandboxed.granted_policy().environment, EnvironmentPolicy::None);
    }

    #[test]
    fn prompt_snippet_reflects_built_configuration() {
        let executor = MontyExecutorBuilder::new()
            .allow_path("/data", "/srv/data", PathAccess::ReadOnly)
            .environ_var("PROJECT", "secret-value")
            .function_fn("noop", "Do nothing.", |_args, _kwargs| async move { Ok(json!(null)) })
            .build_repl()
            .unwrap();
        let snippet = executor.prompt_snippet().expect("monty executors are self-describing");
        assert!(snippet.contains("/data (read-only)"));
        assert!(snippet.contains("PROJECT"));
        assert!(!snippet.contains("secret-value"));
        assert!(snippet.contains("def noop(...):"));
    }
}
