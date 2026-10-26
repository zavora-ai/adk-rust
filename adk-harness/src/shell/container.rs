//! The container shell (spec 3.1 §7.9 "Container", feature `docker`).
//!
//! One container per (app, user, session), provisioned through
//! `SandboxClient::provision`/`start` on the session's first command, kept across runs and
//! stopped when it has been idle for [`ContainerConfig::idle_ttl`] or when the harness
//! closes. The host workspace is bind-mounted at `/workspace` (adk-sandbox S4), so the host
//! file tools and the shell see the same files, and every protected path that exists is
//! mounted read-only over it, which is the container's form of S3.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_devtools::Workspace;
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{
    DockerClient, ExecOutput, Manifest, SandboxClient, SandboxSession, SessionHandle,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::protect::protected_paths;
use super::{
    FileToolGate, IsolationStatus, ShellLedger, ShellLedgerEntry, ShellOutputs, sandbox_unavailable,
};
use crate::audit::{HARNESS_AUDIT_KIND, HarnessAudit};
use crate::policy::has_harness_layer;
use crate::tools::{GOVERNANCE_POLL_INTERVAL, HarnessBashTool, denial_suspected};

/// The image of a session's container unless [`ContainerConfig::image`] names another.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::DEFAULT_CONTAINER_IMAGE, "ubuntu:22.04");
/// ```
pub const DEFAULT_CONTAINER_IMAGE: &str = "ubuntu:22.04";

/// How long a session's container stays up after its last command unless
/// [`ContainerConfig::idle_ttl`] says otherwise.
const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

/// Where the host workspace is mounted inside the container (adk-sandbox S4).
const CONTAINER_WORKSPACE: &str = "/workspace";

/// The Docker daemon address when `DOCKER_HOST` is unset.
const DEFAULT_DOCKER_HOST: &str = "unix:///var/run/docker.sock";

/// Configuration of the container shell (spec 3.1 §7.9 "Container").
///
/// Without a `client`, the harness builds a `DockerClient` for each session's container
/// with the host workspace mounted read-write at `/workspace`, every protected path that
/// exists mounted read-only over it, no network, and the current user's `uid:gid`. A
/// `client` of your own is used as it is: it must mount the workspace at its root itself
/// and run its containers as a user that may write it (`DockerClient::with_user`; root
/// without capabilities cannot write a directory only the host user may write), and its
/// containers are still checked for read-only protected paths before a command runs.
///
/// # Example
///
/// ```rust
/// use adk_harness::{ContainerConfig, DEFAULT_CONTAINER_IMAGE};
///
/// let config = ContainerConfig::default();
/// assert_eq!(config.image, DEFAULT_CONTAINER_IMAGE);
/// assert!(!config.network);
/// assert!(config.client.is_none());
/// ```
#[derive(Clone)]
pub struct ContainerConfig {
    /// The image of every session's container.
    pub image: String,
    /// Allow network access (Docker's `bridge` network). `false`: the container has no
    /// network interface besides loopback.
    pub network: bool,
    /// Extra host paths mounted read-only, as (host path, container path).
    pub extra_read: Vec<(PathBuf, String)>,
    /// Extra host paths mounted read-write, as (host path, container path).
    pub extra_write: Vec<(PathBuf, String)>,
    /// The user commands run as inside the container (`uid`, `uid:gid`, or a name the image
    /// knows). `None`: the current user's `uid:gid` on unix, so files the container writes
    /// into the workspace belong to the person running the harness.
    pub user: Option<String>,
    /// Memory limit in bytes.
    pub memory_limit_bytes: Option<u64>,
    /// CPU limit in cores.
    pub cpu_limit: Option<f64>,
    /// How long a session's container stays up after its last command.
    pub idle_ttl: Duration,
    /// The sandbox client; `None` builds a `DockerClient` per session as described above.
    pub client: Option<Arc<dyn SandboxClient>>,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            image: DEFAULT_CONTAINER_IMAGE.to_string(),
            network: false,
            extra_read: Vec::new(),
            extra_write: Vec::new(),
            user: None,
            memory_limit_bytes: None,
            cpu_limit: None,
            idle_ttl: DEFAULT_IDLE_TTL,
            client: None,
        }
    }
}

impl fmt::Debug for ContainerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContainerConfig")
            .field("image", &self.image)
            .field("network", &self.network)
            .field("extra_read", &self.extra_read)
            .field("extra_write", &self.extra_write)
            .field("user", &self.user)
            .field("memory_limit_bytes", &self.memory_limit_bytes)
            .field("cpu_limit", &self.cpu_limit)
            .field("idle_ttl", &self.idle_ttl)
            .field("client", &self.client.as_ref().map(|_| "custom"))
            .finish()
    }
}

impl ContainerConfig {
    /// Digest of the configuration: image, network, user, extra mounts, limits and whether a
    /// custom client is in use, as `sha256:<hex>` (spec 3.1 §9.2 I9).
    ///
    /// The `bash` declaration's annotations carry it, so standing rules and held calls bound
    /// to one container configuration never match another, nor the host or OS-sandbox shell.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::ContainerConfig;
    ///
    /// let closed = ContainerConfig::default();
    /// let open = ContainerConfig { network: true, ..ContainerConfig::default() };
    /// assert_ne!(closed.profile_digest(), open.profile_digest());
    /// ```
    pub fn profile_digest(&self) -> String {
        let mounts = |mounts: &[(PathBuf, String)]| {
            let mut mounts: Vec<String> = mounts
                .iter()
                .map(|(host, container)| format!("{}:{container}", host.display()))
                .collect();
            mounts.sort();
            mounts
        };
        let profile = json!({
            "enforcer": "container",
            "image": self.image,
            "network": self.network,
            "user": self.user,
            "extraRead": mounts(&self.extra_read),
            "extraWrite": mounts(&self.extra_write),
            "memoryLimitBytes": self.memory_limit_bytes,
            "cpuLimit": self.cpu_limit,
            "customClient": self.client.is_some(),
        });
        format!("sha256:{}", crate::fsutil::sha256_hex(&profile.to_string()))
    }

    /// The user the container runs as: the configured one, or the current `uid:gid`.
    fn user(&self) -> Option<String> {
        if self.user.is_some() {
            return self.user.clone();
        }
        #[cfg(unix)]
        {
            // SAFETY: getuid and getgid take no arguments and cannot fail.
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            Some(format!("{uid}:{gid}"))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Whether the container runtime is reachable (spec 3.1 §7.9 "Availability").
///
/// A custom client was built by the caller, which proved the runtime then. Otherwise the
/// Docker daemon named by `DOCKER_HOST` (default `unix:///var/run/docker.sock`) must accept
/// a connection; the check is synchronous because `build()` is.
pub(crate) fn probe(config: &ContainerConfig) -> std::result::Result<(), String> {
    if config.client.is_some() {
        return Ok(());
    }
    let host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| DEFAULT_DOCKER_HOST.to_string());
    let unreachable = |error: std::io::Error| {
        format!(
            "the Docker daemon at {host} is not reachable ({error}); start Docker or set \
             DOCKER_HOST to the daemon's address"
        )
    };
    if let Some(path) = host.strip_prefix("unix://") {
        #[cfg(unix)]
        {
            std::os::unix::net::UnixStream::connect(path).map(drop).map_err(unreachable)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(format!("a unix socket ({host}) cannot be reached on this platform"))
        }
    } else if let Some(address) =
        host.strip_prefix("tcp://").or_else(|| host.strip_prefix("http://"))
    {
        use std::net::ToSocketAddrs;
        let address = address.trim_end_matches('/');
        let resolved = address
            .to_socket_addrs()
            .map_err(unreachable)?
            .next()
            .ok_or_else(|| format!("DOCKER_HOST {host} names no address"))?;
        std::net::TcpStream::connect_timeout(&resolved, Duration::from_secs(2))
            .map(drop)
            .map_err(unreachable)
    } else {
        Err(format!("DOCKER_HOST {host} is not a unix:// or tcp:// address"))
    }
}

/// Why container commands cannot run unapproved, or `None` when they can
/// (spec 3.1 §7.9, §9.2 I26).
pub(crate) fn auto_allow_refusal(config: &ContainerConfig, root: &Path) -> Option<String> {
    if config.network {
        return Some("the container allows the network".to_string());
    }
    let protected = protected_paths(root);
    (!protected.unprotectable.is_empty()).then(|| {
        format!(
            "these protected paths cannot be kept read-only inside the container: {}; create \
             them (a protected path that does not exist cannot be protected from creation) or \
             replace the symlinks",
            protected.describe_unprotectable()
        )
    })
}

/// Kills every process in the container but its own (PID 1) and this script's shell and its
/// parent, so nothing a command started outlives it (spec 3.1 §9.2 I23). `kill` is a shell
/// builtin, so the sweep forks nothing that it would then have to find.
const REAP_SCRIPT: &str = r#"for p in /proc/[0-9]*; do pid=${p#/proc/}; case "$pid" in 1|"$$"|"$PPID") ;; *) kill -9 "$pid" 2>/dev/null ;; esac; done; exit 0"#;

/// A session's container: the client that made it, its handle, and its live session.
struct Container {
    client: Arc<dyn SandboxClient>,
    handle: SessionHandle,
    session: Arc<dyn SandboxSession>,
    last_used: Mutex<Instant>,
    in_use: AtomicUsize,
    /// The harness built this container, so a `/proc` sweep inside it cannot reach the host;
    /// a caller's own client may run sessions on the host, where the sweep is never run.
    reap: bool,
    /// One command at a time per container, so a reap never ends another command.
    exec: tokio::sync::Mutex<()>,
}

/// A container checked out for one command; dropping it records the use.
pub(crate) struct Lease {
    container: Arc<Container>,
}

impl Lease {
    fn new(container: Arc<Container>) -> Self {
        container.in_use.fetch_add(1, Ordering::SeqCst);
        Self { container }
    }

    /// Runs `command` in the workspace, then ends every process it left behind (I23).
    pub(crate) async fn run(&self, command: &str) -> std::result::Result<ExecOutput, SandboxError> {
        let _one_at_a_time = self.container.exec.lock().await;
        let session = self.container.session.as_ref();
        let output = session.exec_command(command, None).await;
        if self.container.reap
            && let Err(error) = session.exec_command(REAP_SCRIPT, None).await
        {
            tracing::warn!(
                container.handle = self.container.handle.as_str(),
                error = %error,
                "could not end the processes a command left in its container"
            );
        }
        output
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        *self.container.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.container.in_use.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The containers of a harness, one per (app, user, session) (spec 3.1 §7.9 "Container").
pub(crate) struct ContainerPool {
    config: ContainerConfig,
    root: PathBuf,
    containers: tokio::sync::Mutex<HashMap<String, Arc<Container>>>,
    sweeper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl fmt::Debug for ContainerPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContainerPool")
            .field("config", &self.config)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn pool_key(app: &str, user: &str, session: &str) -> String {
    format!("{app}\u{1f}{user}\u{1f}{session}")
}

impl ContainerPool {
    pub(crate) fn new(config: ContainerConfig, root: &Path) -> Self {
        Self {
            config,
            root: root.to_path_buf(),
            containers: tokio::sync::Mutex::new(HashMap::new()),
            sweeper: Mutex::new(None),
        }
    }

    /// The session's container, provisioned on first use.
    ///
    /// # Errors
    ///
    /// Returns why the container could not be provisioned, started, or trusted to keep
    /// protected paths read-only; the caller refuses the command (spec 3.1 §9.2 I24).
    pub(crate) async fn lease(
        self: &Arc<Self>,
        app: &str,
        user: &str,
        session: &str,
    ) -> std::result::Result<Lease, String> {
        self.start_sweeper();
        let key = pool_key(app, user, session);
        let mut containers = self.containers.lock().await;
        if let Some(container) = containers.get(&key) {
            return Ok(Lease::new(container.clone()));
        }
        let container = self.provision().await?;
        containers.insert(key, container.clone());
        Ok(Lease::new(container))
    }

    /// Stops the session's container, if any; the next command provisions a new one.
    pub(crate) async fn stop(&self, app: &str, user: &str, session: &str) {
        let removed = self.containers.lock().await.remove(&pool_key(app, user, session));
        if let Some(container) = removed {
            stop_container(&container).await;
        }
    }

    /// Stops every container (`Harness::close`, spec 3.1 §7.9).
    pub(crate) async fn close(&self) {
        if let Some(sweeper) = self.sweeper.lock().unwrap_or_else(|e| e.into_inner()).take() {
            sweeper.abort();
        }
        let all: Vec<Arc<Container>> =
            self.containers.lock().await.drain().map(|(_, c)| c).collect();
        for container in all {
            stop_container(&container).await;
        }
    }

    /// Stops the containers no command has used for `idle_ttl`.
    async fn sweep(&self) {
        let ttl = self.config.idle_ttl;
        let idle: Vec<Arc<Container>> = {
            let mut containers = self.containers.lock().await;
            let idle_keys: Vec<String> = containers
                .iter()
                .filter(|(_, container)| {
                    container.in_use.load(Ordering::SeqCst) == 0
                        && container.last_used.lock().unwrap_or_else(|e| e.into_inner()).elapsed()
                            > ttl
                })
                .map(|(key, _)| key.clone())
                .collect();
            idle_keys.into_iter().filter_map(|key| containers.remove(&key)).collect()
        };
        for container in idle {
            tracing::info!(
                container.handle = container.handle.as_str(),
                "stopping an idle session container"
            );
            stop_container(&container).await;
        }
    }

    fn start_sweeper(self: &Arc<Self>) {
        let mut sweeper = self.sweeper.lock().unwrap_or_else(|e| e.into_inner());
        if sweeper.is_some() {
            return;
        }
        let period =
            (self.config.idle_ttl / 4).clamp(Duration::from_millis(50), Duration::from_secs(30));
        let pool: Weak<Self> = Arc::downgrade(self);
        *sweeper = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(period).await;
                let Some(pool) = pool.upgrade() else { break };
                pool.sweep().await;
            }
        }));
    }

    async fn provision(&self) -> std::result::Result<Arc<Container>, String> {
        let (client, reap): (Arc<dyn SandboxClient>, bool) = match &self.config.client {
            Some(client) => (client.clone(), false),
            None => (Arc::new(self.docker_client().await?), true),
        };
        let handle = client
            .provision(&Manifest { entries: vec![] })
            .await
            .map_err(|error| format!("the session container could not be provisioned: {error}"))?;
        let session: Arc<dyn SandboxSession> = match client.start(&handle).await {
            Ok(session) => Arc::from(session),
            Err(error) => {
                let _ = client.stop(&handle).await;
                return Err(format!("the session container could not be started: {error}"));
            }
        };
        let protected = protected_paths(&self.root).present;
        let targets: Vec<String> = protected
            .iter()
            .filter_map(|path| {
                path.strip_prefix(&self.root)
                    .ok()
                    .map(|relative| relative.to_string_lossy().into_owned())
            })
            .collect();
        if let Err(reason) = probe_protected(session.as_ref(), &targets).await {
            let _ = client.stop(&handle).await;
            return Err(reason);
        }
        tracing::info!(container.handle = handle.as_str(), image = %self.config.image, "started a session container");
        Ok(Arc::new(Container {
            client,
            handle,
            session,
            last_used: Mutex::new(Instant::now()),
            in_use: AtomicUsize::new(0),
            reap,
            exec: tokio::sync::Mutex::new(()),
        }))
    }

    /// A Docker client for one session's container: the workspace at `/workspace`
    /// read-write, its protected paths read-only over it, the extra mounts, no network
    /// unless allowed, and the configured user (adk-sandbox S4).
    async fn docker_client(&self) -> std::result::Result<DockerClient, String> {
        let mut client = DockerClient::with_image(&self.config.image)
            .await
            .map_err(|error| format!("no Docker daemon: {error}"))?
            .with_network_mode(if self.config.network { "bridge" } else { "none" })
            .with_resource_limits(self.config.memory_limit_bytes, self.config.cpu_limit)
            .with_bind_mount(&self.root, CONTAINER_WORKSPACE, false);
        for path in protected_paths(&self.root).present {
            let relative = path.strip_prefix(&self.root).map_err(|_| {
                format!("protected path {} is outside the workspace", path.display())
            })?;
            let relative = relative
                .to_str()
                .ok_or_else(|| format!("protected path {} is not valid UTF-8", path.display()))?;
            let target = format!("{CONTAINER_WORKSPACE}/{}", relative.replace('\\', "/"));
            client = client.with_bind_mount(&path, target, true);
        }
        for (host, target) in &self.config.extra_read {
            client = client.with_bind_mount(host, target.clone(), true);
        }
        for (host, target) in &self.config.extra_write {
            client = client.with_bind_mount(host, target.clone(), false);
        }
        if let Some(user) = self.config.user() {
            client = client.with_user(user);
        }
        Ok(client)
    }
}

async fn stop_container(container: &Container) {
    if let Err(error) = container.client.stop(&container.handle).await {
        tracing::warn!(
            container.handle = container.handle.as_str(),
            error = %error,
            "could not stop a session container"
        );
    }
}

/// Quotes `value` for `sh`.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Tries to write each protected path from inside the container (the container's S3
/// check, spec 3.1 §9.2 I26, I29): a write that gets through is a breach.
///
/// Paths are relative to the workspace root, which is the command's working directory.
async fn probe_protected(
    session: &dyn SandboxSession,
    targets: &[String],
) -> std::result::Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let list = targets.iter().map(|target| shell_quote(target)).collect::<Vec<_>>().join(" ");
    let script = format!(
        "for p in {list}; do \
           if [ -d \"$p\" ]; then \
             if ( : > \"$p/.adk-harness-probe\" ) 2>/dev/null; then rm -f \"$p/.adk-harness-probe\"; echo \"BREACH $p\"; fi; \
           elif ( : >> \"$p\" ) 2>/dev/null; then echo \"BREACH $p\"; fi; \
         done; echo DONE"
    );
    let output = session.exec_command(&script, None).await.map_err(|error| {
        format!("the protected-path probe could not run in the container: {error}")
    })?;
    if let Some(breach) = output.stdout.lines().find(|line| line.starts_with("BREACH ")) {
        return Err(format!(
            "the container let a command write the protected path {}; refusing to run \
             commands in it",
            breach.trim_start_matches("BREACH ")
        ));
    }
    if !output.stdout.contains("DONE") {
        return Err(format!(
            "the protected-path probe did not complete in the container: {}",
            output.stderr.trim()
        ));
    }
    Ok(())
}

/// The goal check's launcher under the container shell: it refuses, since the check would
/// otherwise run model-written code on the host (spec 3.1 §7.11, §9.2 I24).
#[derive(Debug)]
pub(crate) struct ContainerCheckRefusal;

impl adk_devtools::CommandLauncher for ContainerCheckRefusal {
    fn command(
        &self,
        _ws: &Workspace,
        _script: &str,
        _ctx: &dyn ToolContext,
    ) -> std::result::Result<tokio::process::Command, adk_devtools::DevToolError> {
        Err(adk_devtools::DevToolError::Other(
            "goal checks cannot run inside the container shell yet, and never run on the host \
             in its place; run goals with ShellIsolation::OsSandbox or the host shell"
                .to_string(),
        ))
    }

    fn isolation(&self) -> &str {
        "container"
    }
}

/// The harness `bash` tool inside a container (spec 3.1 §7.9 "Container").
///
/// Each session's commands run in that session's container through
/// `SandboxSession::exec_command`, in `/workspace`, where the host workspace is mounted, with
/// the workspace's timeout ceiling and a post-hoc output cap. The tool adds what
/// [`HarnessBashTool`] adds on the host and in the OS sandbox: the declaration
/// `{ command, timeout_secs? }` (plus `unsandboxed`, `reason` and `retry_of` when
/// unsandboxed retries are on), annotations of `{enforcer, profileDigest, network}`, the
/// refusal to run outside a harness run, a kill-switch check every 250 ms (a freeze stops
/// the container), `sandbox: { enforcer, denialSuspected, hint }` on a failure, the
/// `harness.bash.*` audit records and the shell ledger entry. A command that times out
/// stops the container, which the next command replaces.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::Tool;
/// use adk_harness::{ContainerBashTool, ContainerConfig, InMemoryShellLedger};
///
/// let ws = adk_devtools::Workspace::new(".").allow_bash(true);
/// let bash = ContainerBashTool::new(ws, ContainerConfig::default(), Arc::new(InMemoryShellLedger::new()));
/// assert_eq!(bash.name(), "bash");
/// assert_eq!(bash.annotations().unwrap()["enforcer"], "container");
/// assert_eq!(bash.annotations().unwrap()["network"], false);
/// assert!(bash.parameters_schema().unwrap()["properties"].get("unsandboxed").is_none());
/// ```
pub struct ContainerBashTool {
    workspace: Workspace,
    pool: Arc<ContainerPool>,
    host: HarnessBashTool,
    status: IsolationStatus,
    allow_unsandboxed_retry: bool,
    ledger: Arc<dyn ShellLedger>,
    annotations: Value,
    description: String,
    /// Held exclusive while a container command runs unapproved (spec 3.1 §9.2 I26).
    file_gate: Option<FileToolGate>,
    /// Keeps the whole output of a call whose result is cut (spec 3.1 §7.33 fix 8).
    outputs: Option<Arc<ShellOutputs>>,
}

impl ContainerBashTool {
    /// A `bash` tool over `workspace` whose commands run in containers configured by
    /// `config`. Unsandboxed retries are off (spec 3.1 §7.9 `allow_unsandboxed_retry`).
    pub fn new(
        workspace: Workspace,
        config: ContainerConfig,
        ledger: Arc<dyn ShellLedger>,
    ) -> Self {
        let status = IsolationStatus::Container { image: config.image.clone() };
        let digest = config.profile_digest();
        let network = config.network;
        let pool = Arc::new(ContainerPool::new(config, workspace.root()));
        let mut tool = Self {
            // The unsandboxed retry path, when allowed: the same ledger-bound, once-only host
            // run as the OS sandbox's (I25).
            host: HarnessBashTool::new(workspace.clone(), status.clone(), false, ledger.clone())
                .allow_unsandboxed_retry(false),
            workspace,
            pool,
            status,
            allow_unsandboxed_retry: false,
            ledger,
            annotations: json!({
                "enforcer": "container",
                "profileDigest": digest,
                "network": network,
                "reads": "confined",
                "msys2Isolated": true,
            }),
            description: String::new(),
            file_gate: None,
            outputs: None,
        };
        tool.declare();
        tool
    }

    /// Keeps the whole output of a call whose result is cut in `outputs`, which
    /// `shell_output` reads (spec 3.1 §7.33 fix 8).
    #[must_use]
    pub fn with_outputs(mut self, outputs: Arc<ShellOutputs>) -> Self {
        self.host = self.host.with_outputs(outputs.clone());
        self.outputs = Some(outputs);
        self
    }

    /// Runs each container command while no host file tool runs (spec 3.1 §9.2 I26).
    #[must_use]
    pub(crate) fn with_file_gate(mut self, gate: Option<FileToolGate>) -> Self {
        self.file_gate = gate;
        self
    }

    /// Whether a failed container command may be retried unsandboxed on the host behind a
    /// non-waivable approval (default `false`).
    #[must_use]
    pub fn allow_unsandboxed_retry(mut self, yes: bool) -> Self {
        self.allow_unsandboxed_retry = yes;
        self.host = HarnessBashTool::new(
            self.workspace.clone(),
            self.status.clone(),
            false,
            self.ledger.clone(),
        )
        .allow_unsandboxed_retry(yes);
        if let Some(outputs) = &self.outputs {
            self.host = self.host.with_outputs(outputs.clone());
        }
        self.declare();
        self
    }

    /// The containers this tool runs commands in; closed with the toolset.
    pub(crate) fn pool(&self) -> Arc<ContainerPool> {
        self.pool.clone()
    }

    fn declare(&mut self) {
        let network = if self.pool.config.network { "network access" } else { "no network" };
        let retry = if self.allow_unsandboxed_retry {
            " If it fails because the container denied something, you may retry it once with \
             unsandboxed: true, retry_of: <the failed call id> and a reason; the user must \
             approve that retry."
        } else {
            ""
        };
        self.description = format!(
            "Run a shell command in /workspace inside a container ({}) with {network}. The host \
             workspace is mounted at /workspace, so files you write there are visible to \
             read_file and the user; AGENTS.md, git's hooks and config, and other protected \
             paths are read-only (the rest of .git is writable, so git commit works), and \
             nothing else on the host is reachable. Has a timeout.{retry}",
            self.pool.config.image
        );
    }
}

fn refuse(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, message)
}

#[async_trait]
impl Tool for ContainerBashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        let mut properties = json!({
            "command": { "type": "string", "description": "The shell command to run." },
            "timeout_secs": { "type": "integer", "description": "Optional timeout in seconds; cannot exceed the workspace limit." }
        });
        if self.allow_unsandboxed_retry {
            properties["unsandboxed"] = json!({ "type": "boolean", "description": "Retry a failed container command on the host. Needs retry_of and reason, and the user's approval." });
            properties["reason"] = json!({ "type": "string", "description": "Why the command needs to run outside the container." });
            properties["retry_of"] = json!({ "type": "string", "description": "Function call id of the failed container call." });
        }
        Some(json!({ "type": "object", "properties": properties, "required": ["command"] }))
    }

    fn annotations(&self) -> Option<&Value> {
        Some(&self.annotations)
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(config) = ctx.run_config() else {
            return Err(refuse("harness.bash.ungoverned", "bash runs only inside a harness run"));
        };
        if !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
            return Err(refuse(
                "harness.bash.ungoverned",
                "bash refuses to run without the harness policy in the run config",
            ));
        }
        config.check_governance()?;
        if args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
            if !self.allow_unsandboxed_retry {
                return Err(refuse(
                    "harness.bash.retry_refused",
                    "unsandboxed retries are turned off for the container shell",
                ));
            }
            return self.host.execute(ctx, args).await;
        }
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("bash needs a command"))?
            .to_string();
        let (app, user, session) =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let call_id = ctx.function_call_id().to_string();
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let enforcer = "container";
        // No host file tool runs while a container command does (I26).
        let _exclusive = match &self.file_gate {
            Some(gate) => Some(gate.exclusive(config).await?),
            None => None,
        };

        let lease = match self.pool.lease(&app, &user, &session).await {
            Ok(lease) => lease,
            Err(reason) => {
                if let Some(audit) = &audit {
                    audit
                        .record(
                            "harness.sandbox.unavailable",
                            json!({ "functionCallId": call_id, "requested": enforcer, "error": reason }),
                        )
                        .await?;
                }
                // A container declaration never falls back to the host (I24).
                return Err(sandbox_unavailable(enforcer, &reason));
            }
        };

        if let Some(audit) = &audit {
            let draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.begin",
            )
            .args_digest(audit.trail().digest(&args))
            .payload_ref(audit.trail().digest(&json!({ "command": command })));
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": enforcer,
                        "profileDigest": self.annotations["profileDigest"].clone(),
                        "network": self.annotations["network"].clone(),
                        "reads": self.annotations["reads"].clone(),
                        "msys2Isolated": self.annotations["msys2Isolated"].clone(),
                        "workspaceAccess": "read_write",
                        "envKeys": Vec::<String>::new(),
                        "retryOf": Value::Null,
                        "goalCheck": false,
                    }),
                )
                .await?;
        }

        // The operator's timeout is a ceiling: a model may ask for less time, never more.
        let limit = self.workspace.bash_timeout_value();
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .map_or(limit, |requested| Duration::from_secs(requested).min(limit));
        let started = Instant::now();
        let mut run = Box::pin(tokio::time::timeout(timeout, lease.run(&command)));
        let mut poll = tokio::time::interval(GOVERNANCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let outcome = loop {
            tokio::select! {
                result = &mut run => break Ok(result),
                _ = poll.tick() => {
                    if let Err(frozen) = config.check_governance() {
                        break Err(frozen);
                    }
                }
            }
        };
        drop(run);
        drop(lease);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let mut timed_out = false;
        let mut killed_by_freeze = false;
        let mut result: Result<Value> = match outcome {
            Err(frozen) => {
                // Stopping the container is what ends the command and its descendants (I20).
                self.pool.stop(&app, &user, &session).await;
                killed_by_freeze = true;
                Err(frozen)
            }
            Ok(Err(_elapsed)) => {
                self.pool.stop(&app, &user, &session).await;
                timed_out = true;
                Err(timeout_error(timeout))
            }
            Ok(Ok(Err(error))) => {
                Err(AdkError::tool(format!("the command could not run in the container: {error}")))
            }
            Ok(Ok(Ok(output))) if output.timed_out => {
                timed_out = true;
                Err(timeout_error(timeout))
            }
            Ok(Ok(Ok(output))) => {
                let bounded = adk_devtools::bound_output(
                    self.outputs
                        .clone()
                        .map(|outputs| outputs as Arc<dyn adk_devtools::OutputStore>),
                    ctx.clone(),
                    &command,
                    output.stdout.as_bytes(),
                    output.stderr.as_bytes(),
                    self.workspace.max_output(),
                );
                let mut value = json!({
                    "command": command,
                    "exit_code": output.exit_code,
                    "stdout": bounded.stdout,
                    "stderr": bounded.stderr,
                    "truncated": bounded.truncated,
                });
                if let Some(id) = bounded.output_id {
                    if let Some(outputs) = &self.outputs {
                        outputs.finish(
                            (&app, &user, &session),
                            &id,
                            output.exit_code.and_then(|code| i32::try_from(code).ok()),
                            started.elapsed(),
                        );
                    }
                    value["output_id"] = Value::String(id);
                }
                Ok(value)
            }
        };
        let exit_code = result.as_ref().ok().and_then(|value| {
            value.get("exit_code").and_then(Value::as_i64).and_then(|code| i32::try_from(code).ok())
        });
        let truncated = result
            .as_ref()
            .ok()
            .and_then(|value| value.get("truncated").and_then(Value::as_bool))
            .unwrap_or(false);
        let mut suspected = false;
        if let Ok(value) = &mut result
            && exit_code != Some(0)
        {
            let output = format!(
                "{}\n{}",
                value.get("stderr").and_then(Value::as_str).unwrap_or_default(),
                value.get("stdout").and_then(Value::as_str).unwrap_or_default()
            );
            suspected = denial_suspected(&output);
            let hint = match (suspected, self.allow_unsandboxed_retry) {
                (true, true) => format!(
                    "The container may have refused this: writes to protected paths and \
                     anything outside /workspace, and network access, are denied. If the \
                     command needs them, retry it once with unsandboxed: true, retry_of: \
                     \"{call_id}\" and a reason; the user must approve the retry."
                ),
                (true, false) => "The container may have refused this: writes to protected \
                                  paths and anything outside /workspace, and network access, \
                                  are denied. Unsandboxed retries are turned off; ask the user \
                                  to run it."
                    .to_string(),
                (false, _) => "The command failed inside the container; nothing in its output \
                               suggests the container refused it."
                    .to_string(),
            };
            value["sandbox"] =
                json!({ "enforcer": enforcer, "denialSuspected": suspected, "hint": hint });
        }

        let mut entry = ShellLedgerEntry::new(&call_id, &command, true, exit_code);
        entry.enforcer = Some(enforcer.to_string());
        entry.denial_suspected = suspected;
        if let Err(error) = self.ledger.record(&app, &user, &session, entry).await {
            tracing::warn!(error = %error, "could not record the bash call in the shell ledger");
        }
        if let Some(audit) = &audit {
            let recorded = match &result {
                Ok(_) if exit_code == Some(0) => AuditRecordOutcome::Succeeded,
                Ok(_) => AuditRecordOutcome::Failed,
                Err(_) if timed_out || killed_by_freeze => AuditRecordOutcome::OutcomeUnknown,
                Err(_) => AuditRecordOutcome::Failed,
            };
            let mut draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.end",
            )
            .outcome(recorded);
            if let Ok(value) = &result {
                draft = draft.payload_ref(audit.trail().digest(value));
            }
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": enforcer,
                        "exitCode": exit_code,
                        "durationMs": duration_ms,
                        "timedOut": timed_out,
                        "killedByFreeze": killed_by_freeze,
                        "truncated": truncated,
                        "denialSuspected": suspected,
                        "retryOf": Value::Null,
                    }),
                )
                .await?;
        }
        result
    }
}

fn timeout_error(timeout: Duration) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Timeout,
        "harness.bash.timeout",
        format!("command timed out after {timeout:?}; the session's container was stopped"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_digest_covers_every_setting() {
        let base = ContainerConfig::default();
        let variants = [
            ContainerConfig { image: "alpine:3.20".into(), ..base.clone() },
            ContainerConfig { network: true, ..base.clone() },
            ContainerConfig { user: Some("root".into()), ..base.clone() },
            ContainerConfig {
                extra_read: vec![(PathBuf::from("/opt/data"), "/data".into())],
                ..base.clone()
            },
            ContainerConfig {
                extra_write: vec![(PathBuf::from("/opt/cache"), "/cache".into())],
                ..base.clone()
            },
            ContainerConfig { memory_limit_bytes: Some(1 << 30), ..base.clone() },
            ContainerConfig { cpu_limit: Some(1.5), ..base.clone() },
        ];
        let digests: Vec<String> =
            std::iter::once(&base).chain(&variants).map(ContainerConfig::profile_digest).collect();
        for (i, a) in digests.iter().enumerate() {
            assert!(a.starts_with("sha256:"));
            for b in &digests[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // The idle TTL changes nothing a command can observe.
        let longer = ContainerConfig { idle_ttl: Duration::from_secs(1), ..base.clone() };
        assert_eq!(base.profile_digest(), longer.profile_digest());
    }

    #[test]
    fn output_is_capped_on_a_character_boundary() {
        assert_eq!(cap_output("héllo".to_string(), 1024), ("héllo".to_string(), false));
        assert_eq!(cap_output("€€€".to_string(), 4), ("€\n…[truncated]".to_string(), true));
    }

    /// Parsed, never run: on the host the sweep would end the host's processes.
    #[cfg(unix)]
    #[test]
    fn the_reap_script_parses() {
        let status = std::process::Command::new("sh")
            .args(["-n", "-c", REAP_SCRIPT])
            .status()
            .expect("sh runs");
        assert!(status.success(), "the reap script does not parse");
    }

    #[test]
    fn the_probe_script_quotes_hostile_names() {
        let quoted = shell_quote("it's; rm -rf /");
        assert_eq!(quoted, "'it'\\''s; rm -rf /'");
    }

    #[test]
    fn a_custom_client_counts_as_reachable() {
        struct Never;
        #[async_trait]
        impl SandboxClient for Never {
            async fn provision(
                &self,
                _: &Manifest,
            ) -> std::result::Result<SessionHandle, adk_sandbox::SandboxError> {
                unreachable!()
            }
            async fn start(
                &self,
                _: &SessionHandle,
            ) -> std::result::Result<Box<dyn SandboxSession>, adk_sandbox::SandboxError>
            {
                unreachable!()
            }
            async fn stop(
                &self,
                _: &SessionHandle,
            ) -> std::result::Result<(), adk_sandbox::SandboxError> {
                unreachable!()
            }
            async fn snapshot(
                &self,
                _: &SessionHandle,
            ) -> std::result::Result<adk_sandbox::workspace::SnapshotId, adk_sandbox::SandboxError>
            {
                unreachable!()
            }
            async fn resume(
                &self,
                _: &adk_sandbox::workspace::SnapshotId,
            ) -> std::result::Result<SessionHandle, adk_sandbox::SandboxError> {
                unreachable!()
            }
        }
        let config =
            ContainerConfig { client: Some(Arc::new(Never)), ..ContainerConfig::default() };
        assert_eq!(probe(&config), Ok(()));
    }

    #[test]
    fn auto_allow_needs_the_network_off_and_every_protected_path() {
        let dir = tempfile::tempdir().unwrap();
        let open = ContainerConfig { network: true, ..ContainerConfig::default() };
        assert_eq!(
            auto_allow_refusal(&open, dir.path()).as_deref(),
            Some("the container allows the network")
        );
        let missing = auto_allow_refusal(&ContainerConfig::default(), dir.path()).unwrap();
        assert!(missing.contains(".git (does not exist)"), "{missing}");
        for name in super::super::SANDBOX_PROTECTED_PATHS {
            if [".git", ".adk", ".skills", ".claude", ".cargo"].contains(name) {
                std::fs::create_dir_all(dir.path().join(name)).unwrap();
            } else {
                std::fs::write(dir.path().join(name), "").unwrap();
            }
        }
        assert_eq!(auto_allow_refusal(&ContainerConfig::default(), dir.path()), None);
    }
}
