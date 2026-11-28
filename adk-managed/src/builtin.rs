//! Where the built-in tools of a [`ManagedAgentDef`](crate::types::ManagedAgentDef) run, and
//! what confines them.
//!
//! Every session has a workspace of its own, `<workspace root>/<session id>/workspace`, and a
//! scratch home beside it. [`BuiltinTools`] decides how `bash` and `code_execution` run:
//!
//! | Isolation | `bash`, `code_execution` | `filesystem` |
//! |-----------|--------------------------|--------------|
//! | [`BuiltinIsolation::OsSandbox`] (default) | Under the platform's OS enforcer — Seatbelt on macOS, bubblewrap on Linux — starting in the session workspace, writing only to it and the scratch home, reading only those and the system runtime, with no network; refused where no enforcer runs, Windows included | The session workspace |
//! | [`BuiltinIsolation::Backend`] | Through the [`SandboxBackend`], which provides its own isolation | The session workspace |
//! | [`BuiltinIsolation::UnsafeUnconfinedHost`] | On the host as the runtime's user, starting in the session workspace; nothing confines them | The session workspace |
//!
//! `filesystem` resolves every path inside the session workspace and refuses one that escapes
//! it, lexically or through a symbolic link (`adk-devtools`' workspace containment). Commands
//! never inherit the runtime's environment: they get an allowlist of variables that are not
//! credentials.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use adk_core::egress::ToolEgress;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext};
use adk_devtools::{
    BashTool, CommandLauncher, DevToolError, ReadFileTool, Workspace, WriteFileTool,
};
use adk_sandbox::sandbox::{
    NetworkSupport, ProxyListener, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder,
    WrappedCommand, sandboxed_command,
};
use adk_sandbox::{ExecRequest, Language, SandboxBackend, SandboxError};
use serde_json::{Value, json};

/// Error code of a built-in tool call refused because no OS sandbox can confine it.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_managed::builtin::SANDBOX_UNAVAILABLE_CODE, "managed.builtin.sandbox_unavailable");
/// ```
pub const SANDBOX_UNAVAILABLE_CODE: &str = "managed.builtin.sandbox_unavailable";

/// Error code of a built-in tool call made outside a session the runtime can give a workspace.
pub const INVALID_SESSION_CODE: &str = "managed.builtin.invalid_session";

/// Error code of a session workspace that cannot be created.
pub const WORKSPACE_CODE: &str = "managed.builtin.workspace";

/// How long a `bash` or `code_execution` call may run unless [`BuiltinTools::with_timeout`]
/// says otherwise. A call may ask for less, never more.
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// The variables a confined command receives from the runtime's environment when set. None is
/// a credential; `PATH`, `HOME` and `TMPDIR` are set by the sandbox itself.
const PASSED_ENV: &[&str] = &["LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ"];

/// The search path inside the OS sandbox: the system's program directories.
const SANDBOX_PATH: &str = if cfg!(target_os = "macos") {
    "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"
} else {
    "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
};

/// What runs the `bash` and `code_execution` built-in tools.
///
/// # Example
///
/// ```rust
/// use adk_managed::builtin::{BuiltinIsolation, BuiltinTools};
///
/// assert!(matches!(BuiltinTools::default().isolation(), BuiltinIsolation::OsSandbox));
/// ```
#[derive(Clone)]
pub enum BuiltinIsolation {
    /// The platform's OS enforcer confines each command to the session workspace; the call is
    /// refused with [`SANDBOX_UNAVAILABLE_CODE`] where none runs. The default.
    OsSandbox,
    /// The backend runs each command and provides its own isolation.
    Backend(Arc<dyn SandboxBackend>),
    /// Each command runs on the host with the runtime's privileges and nothing confines it: it
    /// can read and write whatever the runtime's user can and reach the network. Only for a
    /// host that is itself a disposable sandbox, such as a single-tenant container.
    UnsafeUnconfinedHost,
}

impl fmt::Debug for BuiltinIsolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OsSandbox => f.write_str("OsSandbox"),
            Self::Backend(backend) => f.debug_tuple("Backend").field(&backend.name()).finish(),
            Self::UnsafeUnconfinedHost => f.write_str("UnsafeUnconfinedHost"),
        }
    }
}

/// How the built-in tools of a managed agent run: their isolation, the root that holds each
/// session's workspace, and their timeout.
///
/// The default confines `bash` and `code_execution` with the platform's OS enforcer
/// ([`BuiltinIsolation::OsSandbox`]) and keeps workspaces in a private temporary directory
/// created on first use. See the [module documentation](self) for what each isolation does.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// use adk_managed::builtin::{BuiltinIsolation, BuiltinTools};
///
/// let tools = BuiltinTools::os_sandbox()
///     .with_workspace_root("/srv/adk/workspaces")
///     .with_timeout(Duration::from_secs(60));
/// assert!(matches!(tools.isolation(), BuiltinIsolation::OsSandbox));
///
/// // Unconfined execution on the host is an explicit, separately named choice.
/// let unconfined = BuiltinTools::unsafe_unconfined_host();
/// assert!(matches!(unconfined.isolation(), BuiltinIsolation::UnsafeUnconfinedHost));
/// ```
#[derive(Clone)]
pub struct BuiltinTools {
    isolation: BuiltinIsolation,
    enforcer: Option<Arc<dyn SandboxEnforcer>>,
    network: bool,
    workspace_root: Option<PathBuf>,
    timeout: Duration,
    resolved: Arc<Resolved>,
}

/// What a [`BuiltinTools`] resolves once and its clones share.
#[derive(Default)]
struct Resolved {
    enforcer: OnceLock<Result<Arc<dyn SandboxEnforcer>, String>>,
    default_root: OnceLock<Result<PathBuf, String>>,
}

impl Default for BuiltinTools {
    fn default() -> Self {
        Self::os_sandbox()
    }
}

impl fmt::Debug for BuiltinTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltinTools")
            .field("isolation", &self.isolation)
            .field("enforcer", &self.enforcer.as_ref().map(|enforcer| enforcer.name().to_string()))
            .field("network", &self.network)
            .field("workspace_root", &self.workspace_root)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl BuiltinTools {
    /// Built-in tools confined by the platform's OS enforcer, refused where none runs.
    pub fn os_sandbox() -> Self {
        Self {
            isolation: BuiltinIsolation::OsSandbox,
            enforcer: None,
            network: false,
            workspace_root: None,
            timeout: DEFAULT_TOOL_TIMEOUT,
            resolved: Arc::default(),
        }
    }

    /// Built-in `bash` and `code_execution` run through `backend`, which provides their
    /// isolation; `filesystem` keeps to the session workspace on this host.
    ///
    /// Check the backend's [`capabilities`](SandboxBackend::capabilities) before relying on
    /// it: a `ProcessBackend` without an enforcer confines nothing.
    pub fn sandbox_backend(backend: Arc<dyn SandboxBackend>) -> Self {
        Self { isolation: BuiltinIsolation::Backend(backend), ..Self::os_sandbox() }
    }

    /// Built-in `bash` and `code_execution` run on the host with the runtime's privileges, and
    /// nothing confines them.
    ///
    /// A command can read and write whatever the runtime's user can and reach the network;
    /// only its starting directory is the session workspace, and its environment is still an
    /// allowlist. Choose it only where the host is itself a disposable sandbox.
    pub fn unsafe_unconfined_host() -> Self {
        Self { isolation: BuiltinIsolation::UnsafeUnconfinedHost, ..Self::os_sandbox() }
    }

    /// Confines commands with `enforcer` instead of the platform's own
    /// (`adk_sandbox::sandbox::get_enforcer`). Used by [`BuiltinIsolation::OsSandbox`] only.
    #[must_use]
    pub fn with_enforcer(mut self, enforcer: Arc<dyn SandboxEnforcer>) -> Self {
        self.enforcer = Some(enforcer);
        self.resolved = Arc::default();
        self
    }

    /// Lets OS-sandboxed commands reach the network (`false` by default). The enforcers cannot
    /// filter destinations, so this opens all of it.
    #[must_use]
    pub fn with_network(mut self, network: bool) -> Self {
        self.network = network;
        self
    }

    /// Keeps each session's workspace under `root`, as `<root>/<session id>/workspace`.
    ///
    /// Without it, workspaces live in a private temporary directory this value creates on
    /// first use, so they do not outlive the process. Every runtime instance that may host the
    /// same sessions needs the same root for them to find their files.
    #[must_use]
    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self.resolved = Arc::default();
        self
    }

    /// The longest a `bash` or `code_execution` call may run ([`DEFAULT_TOOL_TIMEOUT`] by
    /// default). A call may ask for less with `timeout_secs`, never more.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replaces the isolation, keeping the workspace root, timeout and enforcer settings.
    #[must_use]
    pub fn with_isolation(mut self, isolation: BuiltinIsolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// What runs `bash` and `code_execution`.
    pub fn isolation(&self) -> &BuiltinIsolation {
        &self.isolation
    }

    /// The configured timeout of `bash` and `code_execution`.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Checks that the built-in tools can run as configured, and names what isolates them:
    /// the enforcer (`seatbelt`, `bubblewrap`), the backend, or `unsafe-unconfined-host`.
    ///
    /// Call it at startup to learn of a missing sandbox before a model calls a tool.
    ///
    /// # Errors
    ///
    /// Returns [`SANDBOX_UNAVAILABLE_CODE`] when the isolation is
    /// [`BuiltinIsolation::OsSandbox`] and no enforcer runs on this host.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::builtin::BuiltinTools;
    ///
    /// let label = BuiltinTools::unsafe_unconfined_host().probe()?;
    /// assert_eq!(label, "unsafe-unconfined-host");
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn probe(&self) -> Result<String, AdkError> {
        match &self.isolation {
            BuiltinIsolation::OsSandbox => Ok(self.enforcer("bash")?.name().to_string()),
            BuiltinIsolation::Backend(backend) => Ok(backend.name().to_string()),
            BuiltinIsolation::UnsafeUnconfinedHost => Ok("unsafe-unconfined-host".to_string()),
        }
    }

    /// The workspace of `session_id`, created when missing.
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_SESSION_CODE`] when `session_id` is not a plain identifier, and
    /// [`WORKSPACE_CODE`] when the directory cannot be created.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::builtin::BuiltinTools;
    ///
    /// let root = tempfile::tempdir()?;
    /// let tools = BuiltinTools::default().with_workspace_root(root.path());
    /// let workspace = tools.session_workspace("session-1")?;
    /// assert!(workspace.ends_with("session-1/workspace"));
    /// assert!(tools.session_workspace("../escape").is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn session_workspace(&self, session_id: &str) -> Result<PathBuf, AdkError> {
        Ok(self.session_dirs(session_id)?.0)
    }

    /// Removes the workspace and scratch home of `session_id`, if this value created them.
    pub(crate) async fn remove_session_workspace(&self, session_id: &str) {
        if check_session_id(session_id).is_err() {
            return;
        }
        let root = match &self.workspace_root {
            Some(root) => root.clone(),
            None => match self.resolved.default_root.get() {
                Some(Ok(root)) => root.clone(),
                _ => return,
            },
        };
        let dir = root.join(session_id);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %dir.display(), error = %error, "could not remove a session workspace");
        }
    }

    /// The root that holds the session directories, created when missing.
    fn root(&self) -> Result<PathBuf, AdkError> {
        if let Some(root) = &self.workspace_root {
            create_private_dir(root)?;
            return std::fs::canonicalize(root).map_err(|error| workspace_error(root, &error));
        }
        // A fresh directory with an unpredictable name, private to this user, so another
        // local user cannot plant the root or a session directory under it.
        self.resolved
            .default_root
            .get_or_init(|| {
                tempfile::Builder::new()
                    .prefix("adk-managed-")
                    .tempdir()
                    .map(tempfile::TempDir::keep)
                    .and_then(std::fs::canonicalize)
                    .map_err(|error| error.to_string())
            })
            .clone()
            .map_err(|reason| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    WORKSPACE_CODE,
                    format!(
                        "no workspace root for the built-in tools could be created in the \
                         temporary directory: {reason}. Set one with \
                         `BuiltinTools::with_workspace_root`"
                    ),
                )
            })
    }

    /// The workspace and scratch home of `session_id`, created when missing.
    fn session_dirs(&self, session_id: &str) -> Result<(PathBuf, PathBuf), AdkError> {
        check_session_id(session_id)?;
        let base = self.root()?.join(session_id);
        create_private_dir(&base)?;
        let prepare = |dir: PathBuf| {
            create_private_dir(&dir)?;
            std::fs::canonicalize(&dir).map_err(|error| workspace_error(&dir, &error))
        };
        Ok((prepare(base.join("workspace"))?, prepare(base.join("home"))?))
    }

    /// The enforcer that confines `tool`'s commands, probed once.
    fn enforcer(&self, tool: &str) -> Result<Arc<dyn SandboxEnforcer>, AdkError> {
        self.resolved
            .enforcer
            .get_or_init(|| match &self.enforcer {
                Some(enforcer) => {
                    enforcer.probe().map_err(|error| error.to_string())?;
                    Ok(Arc::clone(enforcer))
                }
                // `get_enforcer` probes the enforcer it selects.
                None => adk_sandbox::sandbox::get_enforcer()
                    .map(Arc::from)
                    .map_err(|error| error.to_string()),
            })
            .clone()
            .map_err(|reason| sandbox_unavailable(tool, &reason))
    }

    /// What `bash` or `code_execution` declares about its network use.
    pub(crate) fn command_egress(&self) -> ToolEgress {
        match self.isolation {
            BuiltinIsolation::OsSandbox if !self.network => ToolEgress::None,
            BuiltinIsolation::OsSandbox
            | BuiltinIsolation::Backend(_)
            | BuiltinIsolation::UnsafeUnconfinedHost => ToolEgress::Undeclared,
        }
    }

    /// Runs the built-in `bash` tool.
    pub(crate) async fn bash(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: &Value,
    ) -> adk_core::Result<Value> {
        let command = args.get("command").and_then(Value::as_str).unwrap_or_default();
        if command.is_empty() {
            return Ok(json!({ "error": "no command provided", "exit_code": 1 }));
        }
        if let BuiltinIsolation::Backend(backend) = &self.isolation {
            return self.run_on_backend(backend, Language::Command, command, args).await;
        }
        self.run_process("bash", ctx, ("sh", "-c"), command, args).await
    }

    /// Runs the built-in `code_execution` tool.
    pub(crate) async fn code_execution(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: &Value,
    ) -> adk_core::Result<Value> {
        let language = args.get("language").and_then(Value::as_str).unwrap_or("python");
        let code = args.get("code").and_then(Value::as_str).unwrap_or_default();
        if code.is_empty() {
            return Ok(json!({ "error": "no code provided" }));
        }
        if let BuiltinIsolation::Backend(backend) = &self.isolation {
            let language = match language {
                "python" | "python3" => Language::Python,
                "javascript" | "js" | "node" => Language::JavaScript,
                "bash" | "sh" => Language::Command,
                "rust" => Language::Rust,
                "typescript" | "ts" => Language::TypeScript,
                other => {
                    return Ok(
                        json!({ "error": format!("unsupported language for the sandbox backend: {other}") }),
                    );
                }
            };
            return self.run_on_backend(backend, language, code, args).await;
        }
        let interpreter = match language {
            "python" | "python3" => ("python3", "-c"),
            "javascript" | "js" | "node" => ("node", "-e"),
            "bash" | "sh" => ("sh", "-c"),
            other => {
                return Ok(json!({
                    "error": format!(
                        "unsupported language: {other}; use python, javascript or bash, or a \
                         sandbox backend for more"
                    )
                }));
            }
        };
        self.run_process("code_execution", ctx, interpreter, code, args).await
    }

    /// Runs the built-in `filesystem` tool inside the session workspace.
    pub(crate) async fn filesystem(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: &Value,
    ) -> adk_core::Result<Value> {
        let operation = args.get("operation").and_then(Value::as_str).unwrap_or("read");
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
        let (root, _) = self.session_dirs(ctx.session_id())?;
        let workspace = Workspace::new(&root);
        match operation {
            "read" => {
                if path.is_empty() {
                    return Ok(json!({ "error": "path is required" }));
                }
                let mut call = json!({ "path": path });
                for key in ["offset", "limit"] {
                    if let Some(value) = args.get(key) {
                        call[key] = value.clone();
                    }
                }
                ReadFileTool::new(workspace).execute(ctx, call).await
            }
            "write" => {
                if path.is_empty() {
                    return Ok(json!({ "error": "path is required" }));
                }
                let content = args.get("content").and_then(Value::as_str).unwrap_or_default();
                WriteFileTool::new(workspace)
                    .execute(ctx, json!({ "path": path, "content": content }))
                    .await
            }
            "list" => {
                let target = if path.is_empty() { "." } else { path };
                let dir = workspace.resolve(target)?;
                let mut entries = tokio::fs::read_dir(&dir)
                    .await
                    .map_err(|error| DevToolError::Other(format!("list failed: {error}")))?;
                let mut files = Vec::new();
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|error| DevToolError::Other(format!("list failed: {error}")))?
                {
                    if !workspace.is_excluded(&entry.path()) {
                        files.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                files.sort();
                Ok(json!({ "path": workspace.display(&dir), "files": files }))
            }
            other => Ok(json!({ "error": format!("unsupported filesystem operation: {other}") })),
        }
    }

    /// Runs `script` with `interpreter` in the session workspace, under the OS sandbox or on
    /// the host, through `adk-devtools`' `BashTool`: timeout ceiling, empty standard input,
    /// and the command's process group stopped when it exits.
    async fn run_process(
        &self,
        tool: &str,
        ctx: Arc<dyn ToolContext>,
        (program, flag): (&'static str, &'static str),
        script: &str,
        args: &Value,
    ) -> adk_core::Result<Value> {
        // Refused before anything is created for the session.
        let enforcer = match self.isolation {
            BuiltinIsolation::OsSandbox => Some(self.enforcer(tool)?),
            BuiltinIsolation::Backend(_) | BuiltinIsolation::UnsafeUnconfinedHost => None,
        };
        let (root, home) = self.session_dirs(ctx.session_id())?;
        let confinement =
            enforcer.map(|enforcer| Confinement { enforcer, home, network: self.network });
        let launcher = Launcher { program, flag, confinement };
        let workspace = Workspace::new(&root).allow_bash(true).bash_timeout(self.timeout);
        let shell = BashTool::new(workspace).with_launcher(Arc::new(launcher));
        let mut call = json!({ "command": script });
        if let Some(seconds) = requested_timeout(args) {
            call["timeout_secs"] = json!(seconds);
        }
        shell.execute(ctx, call).await
    }

    /// Runs `code` through the sandbox backend.
    async fn run_on_backend(
        &self,
        backend: &Arc<dyn SandboxBackend>,
        language: Language,
        code: &str,
        args: &Value,
    ) -> adk_core::Result<Value> {
        let timeout = requested_timeout(args)
            .map_or(self.timeout, |seconds| Duration::from_secs(seconds).min(self.timeout));
        let request = ExecRequest {
            language,
            code: code.to_string(),
            stdin: args.get("stdin").and_then(Value::as_str).map(String::from),
            timeout,
            memory_limit_mb: args
                .get("memory_limit_mb")
                .and_then(Value::as_u64)
                .and_then(|limit| u32::try_from(limit).ok()),
            env: std::collections::HashMap::new(),
        };
        match backend.execute(request).await {
            Ok(result) => Ok(json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "duration_ms": u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
            })),
            Err(error) => Ok(json!({
                "error": format!("sandbox execution failed: {error}"),
                "exit_code": -1,
            })),
        }
    }
}

/// The timeout a call asks for, in seconds: `timeout_secs`, or the older `timeout`.
fn requested_timeout(args: &Value) -> Option<u64> {
    args.get("timeout_secs").or_else(|| args.get("timeout")).and_then(Value::as_u64)
}

/// Refuses a session ID that is not a plain directory name.
fn check_session_id(session_id: &str) -> Result<(), AdkError> {
    let plain = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id != "."
        && session_id != ".."
        && session_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        return Ok(());
    }
    Err(AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        INVALID_SESSION_CODE,
        format!(
            "built-in tools run in a session's workspace, and '{session_id}' cannot name one: a \
             session ID is 1 to 128 ASCII letters, digits, '-', '_' or '.'"
        ),
    ))
}

/// Creates `dir` and its parents, readable by this user only.
fn create_private_dir(dir: &Path) -> Result<(), AdkError> {
    std::fs::create_dir_all(dir).map_err(|error| workspace_error(dir, &error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| workspace_error(dir, &error))?;
    }
    Ok(())
}

fn workspace_error(dir: &Path, error: &std::io::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Internal,
        WORKSPACE_CODE,
        format!("the session workspace '{}' could not be prepared: {error}", dir.display()),
    )
}

fn sandbox_unavailable(tool: &str, reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unsupported,
        SANDBOX_UNAVAILABLE_CODE,
        format!(
            "the built-in `{tool}` tool runs confined by an OS sandbox (Seatbelt on macOS, \
             bubblewrap on Linux) and none is available here: {reason}. On Linux, install \
             bubblewrap and allow unprivileged user namespaces. On Windows, or wherever no \
             enforcer runs, give the runtime a sandbox backend (`BuiltinTools::sandbox_backend`), \
             or opt in to running built-in tools on the host with nothing confining them \
             (`BuiltinTools::unsafe_unconfined_host`)"
        ),
    )
}

/// The OS sandbox one command runs under.
struct Confinement {
    enforcer: Arc<dyn SandboxEnforcer>,
    home: PathBuf,
    network: bool,
}

impl Confinement {
    /// The policy of a command in `workspace`: the workspace and the scratch home read-write,
    /// the system runtime read-only, the network denied unless opened.
    fn policy(&self, workspace: &Path) -> SandboxPolicy {
        let home = self.home.display().to_string();
        let mut builder = SandboxPolicyBuilder::new()
            .allow_process_spawn()
            .allow_read_write(workspace)
            .allow_read_write(&self.home)
            .env("PATH", SANDBOX_PATH)
            .env("HOME", home.clone())
            .env("TMPDIR", home);
        for key in PASSED_ENV {
            if let Ok(value) = std::env::var(key) {
                builder = builder.env(*key, value);
            }
        }
        for path in &system_runtime().read {
            builder = builder.allow_read(path);
        }
        if self.network {
            builder = builder.allow_network();
        }
        builder.build()
    }

    /// The enforcer that builds the command: bubblewrap also gets `/dev`, `/proc` and the
    /// merged-`/usr` symbolic links a system's programs and dynamic loader are found through.
    fn command_enforcer(&self) -> Arc<dyn SandboxEnforcer> {
        if self.enforcer.name() == "bubblewrap" {
            Arc::new(LinkingEnforcer {
                inner: Arc::clone(&self.enforcer),
                links: system_runtime().links.clone(),
            })
        } else {
            Arc::clone(&self.enforcer)
        }
    }
}

/// What a confined command reads of the system: nothing on macOS, where Seatbelt grants the
/// system runtime itself; on Linux, where bubblewrap starts from an empty root, `/usr`, `/etc`
/// and the top-level program and library directories, with those that are symbolic links into
/// `/usr` recreated as links.
#[derive(Default)]
struct SystemRuntime {
    read: Vec<PathBuf>,
    links: Vec<(OsString, PathBuf)>,
}

fn system_runtime() -> &'static SystemRuntime {
    static SYSTEM: OnceLock<SystemRuntime> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        let mut system = SystemRuntime::default();
        if !cfg!(target_os = "linux") {
            return system;
        }
        for dir in ["/usr", "/etc"] {
            if Path::new(dir).is_dir() {
                system.read.push(PathBuf::from(dir));
            }
        }
        for dir in ["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"] {
            let path = PathBuf::from(dir);
            let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue };
            if metadata.file_type().is_symlink() {
                if let (Ok(text), Ok(target)) =
                    (std::fs::read_link(&path), std::fs::canonicalize(&path))
                {
                    system.read.push(target);
                    system.links.push((text.into_os_string(), path));
                }
            } else if metadata.is_dir() {
                system.read.push(path);
            }
        }
        system
    })
}

/// bubblewrap with `/dev`, `/proc` and the given symbolic links added before the command.
struct LinkingEnforcer {
    inner: Arc<dyn SandboxEnforcer>,
    links: Vec<(OsString, PathBuf)>,
}

impl SandboxEnforcer for LinkingEnforcer {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn probe(&self) -> Result<(), SandboxError> {
        self.inner.probe()
    }

    fn network_support(&self) -> NetworkSupport {
        self.inner.network_support()
    }

    fn proxy_listener(&self) -> ProxyListener {
        self.inner.proxy_listener()
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        let mut wrapped = self.inner.wrap_command(program, args, policy)?;
        let separator =
            wrapped.args.iter().position(|arg| arg == "--").unwrap_or(wrapped.args.len());
        let mut extra: Vec<OsString> =
            ["--dev", "/dev", "--proc", "/proc"].into_iter().map(OsString::from).collect();
        for (text, link) in &self.links {
            extra.push(OsString::from("--symlink"));
            extra.push(text.clone());
            extra.push(link.clone().into_os_string());
        }
        wrapped.args.splice(separator..separator, extra);
        Ok(wrapped)
    }

    fn configure_command(
        &self,
        cmd: &mut tokio::process::Command,
        policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        self.inner.configure_command(cmd, policy)
    }
}

/// Builds the process of one `bash` or `code_execution` call for `BashTool`.
struct Launcher {
    program: &'static str,
    flag: &'static str,
    confinement: Option<Confinement>,
}

impl CommandLauncher for Launcher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        let args = [OsString::from(self.flag), OsString::from(script)];
        let Some(confinement) = &self.confinement else {
            let mut command = tokio::process::Command::new(self.program);
            // The runtime's environment holds provider keys; only the workspace's allowlist
            // passes.
            command.args(&args).current_dir(ws.root()).env_clear().envs(ws.bash_env());
            return Ok(command);
        };
        let policy = confinement.policy(ws.root());
        sandboxed_command(
            confinement.command_enforcer().as_ref(),
            OsStr::new(self.program),
            &args,
            &policy,
            ws.root(),
        )
        .map_err(|error| {
            DevToolError::Other(format!(
                "the OS sandbox ({}) could not confine the command, so it did not run: {error}",
                confinement.enforcer.name()
            ))
        })
    }

    fn isolation(&self) -> &str {
        match &self.confinement {
            Some(confinement) => confinement.enforcer.name(),
            None => "unsafe-unconfined-host",
        }
    }

    fn egress(&self) -> ToolEgress {
        match &self.confinement {
            Some(confinement) if !confinement.network => ToolEgress::None,
            Some(_) | None => ToolEgress::Undeclared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps nothing, as bubblewrap's argument shape: `bwrap <args> -- <program> <args>`.
    struct FakeBubblewrap;

    impl SandboxEnforcer for FakeBubblewrap {
        fn name(&self) -> &str {
            "bubblewrap"
        }
        fn probe(&self) -> Result<(), SandboxError> {
            Ok(())
        }
        fn network_support(&self) -> NetworkSupport {
            NetworkSupport { denied: true, proxied: false }
        }
        fn wrap_command(
            &self,
            program: &OsStr,
            args: &[OsString],
            _policy: &SandboxPolicy,
        ) -> Result<WrappedCommand, SandboxError> {
            let mut wrapped: Vec<OsString> =
                ["--ro-bind", "/usr", "/usr", "--"].into_iter().map(OsString::from).collect();
            wrapped.push(program.to_owned());
            wrapped.extend_from_slice(args);
            Ok(WrappedCommand { program: "bwrap".into(), args: wrapped })
        }
    }

    #[test]
    fn bubblewrap_gets_devices_and_links_before_the_separator() {
        let enforcer = LinkingEnforcer {
            inner: Arc::new(FakeBubblewrap),
            links: vec![(OsString::from("usr/lib64"), PathBuf::from("/lib64"))],
        };
        let wrapped = enforcer
            .wrap_command(
                OsStr::new("sh"),
                &[OsString::from("-c"), OsString::from("true")],
                &SandboxPolicyBuilder::new().build(),
            )
            .unwrap();
        let args: Vec<String> =
            wrapped.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "--ro-bind",
                "/usr",
                "/usr",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--symlink",
                "usr/lib64",
                "/lib64",
                "--",
                "sh",
                "-c",
                "true",
            ]
        );
    }

    #[test]
    fn the_policy_grants_the_workspace_and_home_and_denies_the_network() {
        let confinement = Confinement {
            enforcer: Arc::new(FakeBubblewrap),
            home: PathBuf::from("/srv/s1/home"),
            network: false,
        };
        let policy = confinement.policy(Path::new("/srv/s1/workspace"));
        let writable: Vec<&Path> = policy
            .allowed_paths
            .iter()
            .filter(|entry| entry.mode == adk_sandbox::sandbox::AccessMode::ReadWrite)
            .map(|entry| entry.path.as_path())
            .collect();
        assert_eq!(writable, [Path::new("/srv/s1/workspace"), Path::new("/srv/s1/home")]);
        assert_eq!(policy.network, adk_sandbox::sandbox::NetworkAccess::Denied);
        assert_eq!(policy.env.get("HOME").map(String::as_str), Some("/srv/s1/home"));
        assert_eq!(policy.env.get("PATH").map(String::as_str), Some(SANDBOX_PATH));
        let credentials: Vec<&String> = policy
            .env
            .keys()
            .filter(|key| !["PATH", "HOME", "TMPDIR"].contains(&key.as_str()))
            .filter(|key| !PASSED_ENV.contains(&key.as_str()))
            .collect();
        assert!(credentials.is_empty(), "{credentials:?}");
    }

    #[test]
    fn a_session_id_must_be_a_plain_name() {
        for valid in ["0b7c-11", "session_1", "a.b"] {
            assert!(check_session_id(valid).is_ok(), "{valid}");
        }
        for invalid in ["", ".", "..", "../x", "a/b", "a\\b", "ä"] {
            let error = check_session_id(invalid).unwrap_err();
            assert_eq!(error.code, INVALID_SESSION_CODE, "{invalid}");
        }
    }
}
