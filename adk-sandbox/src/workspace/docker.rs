//! Docker sandbox client implementation.
//!
//! Provisions workspaces inside Docker containers from a configurable base
//! image. Provides stronger isolation via container boundaries.
//!
//! This module is gated behind the `workspace-docker` feature flag.
//!
//! ## Container Defaults
//!
//! | Setting | Default | Override |
//! |---------|---------|----------|
//! | Network | `none` — no network interface besides loopback | [`DockerClient::with_network_mode`] |
//! | Capabilities | All dropped (`cap_drop: ALL`) | — |
//! | Privilege escalation | `no-new-privileges` | — |
//! | Process count | [`DEFAULT_PIDS_LIMIT`] | [`DockerClient::with_pids_limit`] |
//! | Memory, CPU | Unlimited | [`DockerClient::with_resource_limits`] |
//! | Host directories | None mounted | [`DockerClient::with_bind_mount`] |
//! | User | The image's default (usually `root`) | [`DockerClient::with_user`] |
//!
//! `GitRepo` manifest entries clone from inside the container, so they need a network mode
//! other than `none`.
//!
//! Paths, URLs, and branch names are passed to commands as separate arguments, never
//! interpolated into a shell script. A command that exceeds its timeout is killed inside the
//! container together with its descendants.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CommitContainerOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use futures::StreamExt;
use tokio::sync::RwLock;

use super::client::SandboxClient;
use super::manifest::{Manifest, ManifestEntry};
use super::path_safety::validate_relative_path;
use super::session::SandboxSession;
use super::types::{DirEntry, EntryType, ExecOutput, SessionHandle, SnapshotId};
use crate::SandboxError;

/// The workspace root directory inside Docker containers.
const CONTAINER_WORKSPACE_ROOT: &str = "/workspace";

/// Default command timeout (120 seconds).
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// Network mode for new containers unless [`DockerClient::with_network_mode`] changes it.
pub const DEFAULT_NETWORK_MODE: &str = "none";

/// Maximum number of processes in a container unless [`DockerClient::with_pids_limit`]
/// changes it. Bounds fork bombs without constraining ordinary builds.
pub const DEFAULT_PIDS_LIMIT: i64 = 512;

/// Upper bound on the in-container cleanup that follows a command timeout.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `$1` as a child of a shell whose PID is recorded in `$2`, so a timeout can find and
/// kill the whole tree. The PID file is removed once the command finishes.
const TIMED_EXEC_SCRIPT: &str =
    r#"{ echo $$ > "$2"; } 2>/dev/null; sh -c "$1"; status=$?; rm -f "$2"; exit $status"#;

/// Kills the process tree whose root PID is recorded in `$1`: its process group, and every
/// descendant found through `/proc`, stopped first so none can fork during the sweep.
const KILL_TREE_SCRIPT: &str = r#"pid=$(cat "$1" 2>/dev/null) || exit 0
rm -f "$1"
tree() { echo "$1"; for child in $(cat /proc/"$1"/task/*/children 2>/dev/null); do tree "$child"; done; }
kill -s STOP $(tree "$pid") 2>/dev/null
kill -s KILL -- -"$pid" $(tree "$pid") 2>/dev/null
exit 0"#;

/// SandboxClient implementation using Docker containers.
///
/// Provisions workspaces inside containers from a configurable base image.
/// Containers start with no network, all capabilities dropped,
/// `no-new-privileges`, and a process limit; see the
/// [module documentation](self) for the defaults and how to change them.
/// Resource limits (memory, CPU) can be configured and are applied to
/// containers on provisioning.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::DockerClient;
///
/// let client = DockerClient::new().await?;
/// let client = client
///     .with_resource_limits(Some(512 * 1024 * 1024), Some(1.5))
///     .with_network_mode("bridge");
/// ```
pub struct DockerClient {
    /// Docker base image for new containers.
    pub base_image: String,
    /// Optional memory limit for containers (in bytes).
    pub memory_limit_bytes: Option<u64>,
    /// Optional CPU limit (fractional cores, e.g., 1.5).
    pub cpu_limit: Option<f64>,
    /// Docker network mode for new containers. Default: [`DEFAULT_NETWORK_MODE`] (`"none"`).
    pub network_mode: String,
    /// Maximum number of processes per container; `None` removes the limit.
    /// Default: [`DEFAULT_PIDS_LIMIT`].
    pub pids_limit: Option<i64>,
    /// The user commands run as inside new containers (`uid`, `uid:gid`, or a name the
    /// image knows); `None` keeps the image's default.
    pub user: Option<String>,
    /// Host paths mounted into every new container, in the order they were added.
    bind_mounts: Vec<BindMount>,
    /// Bollard Docker client for API communication.
    client: Docker,
    /// Active sessions mapping handle IDs to container IDs.
    sessions: RwLock<HashMap<String, String>>,
}

impl DockerClient {
    /// Creates a new `DockerClient` connected to the local Docker daemon.
    ///
    /// Uses the default Docker socket connection (typically
    /// `/var/run/docker.sock` on Unix). The default base image is
    /// `ubuntu:22.04`.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::DockerUnavailable` if the Docker daemon
    /// is not accessible.
    pub async fn new() -> Result<Self, SandboxError> {
        let docker =
            Docker::connect_with_local_defaults().map_err(|e| SandboxError::DockerUnavailable {
                reason: format!("failed to connect to Docker daemon: {e}"),
            })?;

        // Verify the connection by pinging the daemon
        docker.ping().await.map_err(|e| SandboxError::DockerUnavailable {
            reason: format!("Docker daemon not responding: {e}"),
        })?;

        Ok(Self {
            base_image: "ubuntu:22.04".to_string(),
            memory_limit_bytes: None,
            cpu_limit: None,
            network_mode: DEFAULT_NETWORK_MODE.to_string(),
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
            user: None,
            bind_mounts: Vec::new(),
            client: docker,
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Creates a new `DockerClient` with a custom base image.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::DockerUnavailable` if the Docker daemon
    /// is not accessible.
    pub async fn with_image(base_image: impl Into<String>) -> Result<Self, SandboxError> {
        let mut client = Self::new().await?;
        client.base_image = base_image.into();
        Ok(client)
    }

    /// Sets resource limits on the client, returning the modified client.
    ///
    /// # Arguments
    ///
    /// * `memory_limit_bytes` - Optional memory limit in bytes for containers.
    /// * `cpu_limit` - Optional CPU limit as fractional cores (e.g., 1.5 = 1.5 cores).
    pub fn with_resource_limits(
        mut self,
        memory_limit_bytes: Option<u64>,
        cpu_limit: Option<f64>,
    ) -> Self {
        self.memory_limit_bytes = memory_limit_bytes;
        self.cpu_limit = cpu_limit;
        self
    }

    /// Sets the Docker network mode for new containers, returning the modified client.
    ///
    /// The default, `"none"`, gives containers only a loopback interface. Use `"bridge"`
    /// (Docker's default network) or a user-defined network name when commands or `GitRepo`
    /// manifest entries need network access.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::workspace::DockerClient;
    ///
    /// let client = DockerClient::new().await?.with_network_mode("bridge");
    /// ```
    pub fn with_network_mode(mut self, network_mode: impl Into<String>) -> Self {
        self.network_mode = network_mode.into();
        self
    }

    /// Sets the per-container process limit, returning the modified client.
    ///
    /// `None` removes the limit. The default is [`DEFAULT_PIDS_LIMIT`].
    pub fn with_pids_limit(mut self, pids_limit: Option<i64>) -> Self {
        self.pids_limit = pids_limit;
        self
    }

    /// Runs commands in every new container as `user` (`uid`, `uid:gid`, or a name the
    /// image knows), returning the modified client.
    ///
    /// A workspace mounted with [`with_bind_mount`](Self::with_bind_mount) keeps the host's
    /// ownership rules. The container's capabilities are dropped, so its root user cannot
    /// write a directory that only the host user may write, and files a root container does
    /// create belong to root on the host, where that user can neither change nor remove them.
    /// Running the container as the host user's `uid:gid` keeps both sides of the mount
    /// writable by the same person (spec 3.1 §10 S4).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::workspace::DockerClient;
    ///
    /// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
    /// let client = DockerClient::new().await?.with_user("1000:1000");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Mounts the host path `host` at `container` in every container this client creates,
    /// returning the modified client.
    ///
    /// Implements spec 3.1 §10 S4: mounting the host workspace at `/workspace` gives
    /// commands in the container and file tools on the host one view of the same files. A
    /// read-only mount refuses every write from inside the container.
    ///
    /// Both paths must be absolute, and `host` must exist and be valid UTF-8; Docker rejects
    /// anything else when the container is created, and provisioning returns
    /// `ProvisionFailed`. The container's capabilities stay dropped, so a process inside
    /// cannot remount a read-only mount writable. Content under a mount stays on the host:
    /// [`SandboxClient::snapshot`] does not capture it, and [`SandboxClient::resume`] mounts
    /// the same host paths again.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_sandbox::workspace::DockerClient;
    ///
    /// # async fn run() -> Result<(), adk_sandbox::SandboxError> {
    /// let client = DockerClient::new()
    ///     .await?
    ///     .with_bind_mount("/home/me/project", "/workspace", false)
    ///     .with_bind_mount("/home/me/.cargo/registry", "/usr/local/cargo/registry", true);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_bind_mount(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
        read_only: bool,
    ) -> Self {
        self.bind_mounts.push(BindMount {
            host: host.into(),
            container: container.into(),
            read_only,
        });
        self
    }

    /// Generates a unique session handle ID.
    fn generate_session_id() -> String {
        format!("docker-session-{}", uuid::Uuid::new_v4())
    }

    /// Builds the `HostConfig` for a new container from this client's settings.
    fn build_host_config(&self) -> Result<HostConfig, SandboxError> {
        host_config(
            self.memory_limit_bytes,
            self.cpu_limit,
            &self.network_mode,
            self.pids_limit,
            &self.bind_mounts,
        )
    }

    /// Executes a command inside a container and returns stdout/stderr.
    async fn exec_in_container(
        &self,
        container_id: &str,
        cmd: Vec<&str>,
        working_dir: Option<&str>,
        stdin_content: Option<&[u8]>,
    ) -> Result<(String, String, i64), SandboxError> {
        let (stdout, stderr, exit_code) =
            exec_collect(&self.client, container_id, cmd, working_dir, stdin_content).await?;
        Ok((
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
        ))
    }
}

/// A host path mounted into every container a [`DockerClient`] creates.
#[derive(Debug, Clone, PartialEq)]
struct BindMount {
    host: PathBuf,
    container: String,
    read_only: bool,
}

/// Builds the `HostConfig` for a sandbox container.
///
/// Every container drops all capabilities and sets `no-new-privileges`; the network mode,
/// process limit, resource limits, and bind mounts come from the caller.
///
/// # Errors
///
/// Returns `ProvisionFailed` when a bind mount's host path is not valid UTF-8, which the
/// Docker API cannot express.
fn host_config(
    memory_limit_bytes: Option<u64>,
    cpu_limit: Option<f64>,
    network_mode: &str,
    pids_limit: Option<i64>,
    bind_mounts: &[BindMount],
) -> Result<HostConfig, SandboxError> {
    let mounts = bind_mounts
        .iter()
        .map(|mount| {
            let source = mount.host.to_str().ok_or_else(|| SandboxError::ProvisionFailed {
                resource: mount.host.display().to_string(),
                reason: "the bind mount's host path is not valid UTF-8".to_string(),
                suggestion: "Rename the directory or mount a parent directory instead; the \
                             Docker API takes UTF-8 paths."
                    .to_string(),
            })?;
            Ok(Mount {
                target: Some(mount.container.clone()),
                source: Some(source.to_string()),
                // A structured mount, unlike a `host:container:ro` bind string, keeps a `:`
                // in either path from being read as an option separator.
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(mount.read_only),
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>, SandboxError>>()?;

    Ok(HostConfig {
        // Docker takes a signed byte count; saturate rather than wrap for absurd limits.
        memory: memory_limit_bytes.map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX)),
        // Docker uses NanoCPUs (1e9 = 1 core)
        nano_cpus: cpu_limit.map(|cpu| (cpu * 1_000_000_000.0) as i64),
        network_mode: Some(network_mode.to_string()),
        cap_drop: Some(vec!["ALL".to_string()]),
        security_opt: Some(vec!["no-new-privileges".to_string()]),
        pids_limit,
        mounts: (!mounts.is_empty()).then_some(mounts),
        ..Default::default()
    })
}

/// The configuration of a sandbox container from `image`: kept alive by `sleep infinity`,
/// started in the workspace root, running as `user` when one is set.
fn container_config(image: &str, user: Option<&str>, host_config: HostConfig) -> Config<String> {
    Config {
        image: Some(image.to_string()),
        cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
        working_dir: Some(CONTAINER_WORKSPACE_ROOT.to_string()),
        user: user.map(str::to_string),
        host_config: Some(host_config),
        ..Default::default()
    }
}

/// Command that writes its stdin to `path` with the path passed as `$1`, so no character
/// in it is interpreted by the shell.
fn write_file_command(path: &str) -> Vec<String> {
    ["sh", "-c", r#"cat > "$1""#, "adk-write", path].map(str::to_string).to_vec()
}

/// Commands that clone `url` into `path` and optionally check out `branch`.
///
/// # Errors
///
/// Returns `ProvisionFailed` when `branch` starts with `-`, which git would parse as an
/// option and which `git check-ref-format --branch` rejects anyway.
fn git_clone_commands(
    url: &str,
    branch: Option<&str>,
    path: &str,
) -> Result<Vec<Vec<String>>, SandboxError> {
    let mut commands = vec![["git", "clone", "--", url, path].map(str::to_string).to_vec()];
    if let Some(branch) = branch {
        if branch.starts_with('-') {
            return Err(SandboxError::ProvisionFailed {
                resource: path.to_string(),
                reason: format!("branch name '{branch}' starts with '-'"),
                suggestion: "Use a valid branch name; git rejects names beginning with a dash."
                    .to_string(),
            });
        }
        commands.push(["git", "-C", path, "checkout", branch].map(str::to_string).to_vec());
    }
    Ok(commands)
}

/// Command that runs `command` under `sh -c` and records the wrapper's PID in `pid_file`.
fn timed_exec_command(command: &str, pid_file: &str) -> Vec<String> {
    ["sh", "-c", TIMED_EXEC_SCRIPT, "adk-exec", command, pid_file].map(str::to_string).to_vec()
}

/// Command that kills the process tree recorded in `pid_file` by [`timed_exec_command`].
fn kill_tree_command(pid_file: &str) -> Vec<String> {
    ["sh", "-c", KILL_TREE_SCRIPT, "adk-kill", pid_file].map(str::to_string).to_vec()
}

/// Runs `cmd` in `container_id` and returns raw stdout, raw stderr, and the exit code.
async fn exec_collect(
    client: &Docker,
    container_id: &str,
    cmd: Vec<&str>,
    working_dir: Option<&str>,
    stdin_content: Option<&[u8]>,
) -> Result<(Vec<u8>, Vec<u8>, i64), SandboxError> {
    let exec_options = CreateExecOptions {
        cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        attach_stdin: stdin_content.is_some().then_some(true),
        working_dir: working_dir.map(|d| d.to_string()),
        ..Default::default()
    };

    let exec = client.create_exec(container_id, exec_options).await.map_err(|e| {
        SandboxError::ExecutionFailed(format!("failed to create exec instance: {e}"))
    })?;

    let start_result = client
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("failed to start exec: {e}")))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    match start_result {
        StartExecResults::Attached { mut output, mut input } => {
            // If we have stdin content, write it and close
            if let Some(content) = stdin_content {
                use tokio::io::AsyncWriteExt;
                let _ = input.write_all(content).await;
                let _ = input.shutdown().await;
            }

            while let Some(msg) = output.next().await {
                match msg {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout.extend_from_slice(&message);
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr.extend_from_slice(&message);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        stderr.extend_from_slice(format!("stream error: {e}").as_bytes());
                    }
                }
            }
        }
        StartExecResults::Detached => {}
    }

    // Get the exit code from the exec inspect
    let inspect = client
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("failed to inspect exec: {e}")))?;

    let exit_code = inspect.exit_code.unwrap_or(-1);

    Ok((stdout, stderr, exit_code))
}

#[async_trait]
impl SandboxClient for DockerClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        // Create container from base image with resource limits
        let host_config = self.build_host_config()?;

        let config = container_config(&self.base_image, self.user.as_deref(), host_config);

        let container = self
            .client
            .create_container(None::<CreateContainerOptions<String>>, config)
            .await
            .map_err(|e| SandboxError::ProvisionFailed {
                resource: self.base_image.clone(),
                reason: format!("failed to create container: {e}"),
                suggestion: "Ensure the base image exists locally or can be pulled.".to_string(),
            })?;

        let container_id = container.id;

        // A failure past this point would leave the created container running with no
        // handle to stop it, so the container is removed before the error is returned.
        let populated = async {
            // Start the container so we can exec into it
            self.client.start_container::<String>(&container_id, None).await.map_err(|e| {
                SandboxError::ProvisionFailed {
                    resource: container_id.clone(),
                    reason: format!("failed to start container: {e}"),
                    suggestion: "Check Docker daemon status and resource availability.".to_string(),
                }
            })?;

            // Create the workspace directory inside the container
            let (_, stderr, exit_code) = self
                .exec_in_container(
                    &container_id,
                    vec!["mkdir", "-p", CONTAINER_WORKSPACE_ROOT],
                    None,
                    None,
                )
                .await?;

            if exit_code != 0 {
                return Err(SandboxError::ProvisionFailed {
                    resource: CONTAINER_WORKSPACE_ROOT.to_string(),
                    reason: format!("failed to create workspace dir: {stderr}"),
                    suggestion: "Check container filesystem permissions.".to_string(),
                });
            }

            // Process each manifest entry
            for entry in &manifest.entries {
                match entry {
                    ManifestEntry::File { path, content } => {
                        validate_relative_path(path)?;
                        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

                        // Create parent directories
                        if let Some(parent_idx) = full_path.rfind('/') {
                            let parent = &full_path[..parent_idx];
                            let (_, _, code) = self
                                .exec_in_container(
                                    &container_id,
                                    vec!["mkdir", "-p", parent],
                                    None,
                                    None,
                                )
                                .await?;
                            if code != 0 {
                                return Err(SandboxError::ProvisionFailed {
                                    resource: path.clone(),
                                    reason: "failed to create parent directories".to_string(),
                                    suggestion: "Check container filesystem permissions.".to_string(),
                                });
                            }
                        }

                        // Write file content from stdin; the path is an argument, never shell text.
                        let write_cmd = write_file_command(&full_path);
                        let (_, stderr, code) = self
                            .exec_in_container(
                                &container_id,
                                write_cmd.iter().map(String::as_str).collect(),
                                None,
                                Some(content),
                            )
                            .await?;
                        if code != 0 {
                            return Err(SandboxError::ProvisionFailed {
                                resource: path.clone(),
                                reason: format!("failed to write file: {stderr}"),
                                suggestion: "Check container filesystem permissions.".to_string(),
                            });
                        }
                    }

                    ManifestEntry::Directory { path } => {
                        validate_relative_path(path)?;
                        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");
                        let (_, stderr, code) = self
                            .exec_in_container(
                                &container_id,
                                vec!["mkdir", "-p", &full_path],
                                None,
                                None,
                            )
                            .await?;
                        if code != 0 {
                            return Err(SandboxError::ProvisionFailed {
                                resource: path.clone(),
                                reason: format!("failed to create directory: {stderr}"),
                                suggestion: "Check container filesystem permissions.".to_string(),
                            });
                        }
                    }

                    ManifestEntry::GitRepo { url, branch, path } => {
                        validate_relative_path(path)?;
                        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

                        for git_cmd in git_clone_commands(url, branch.as_deref(), &full_path)? {
                            let (_, stderr, code) = self
                                .exec_in_container(
                                    &container_id,
                                    git_cmd.iter().map(String::as_str).collect(),
                                    None,
                                    None,
                                )
                                .await?;
                            if code != 0 {
                                return Err(SandboxError::ProvisionFailed {
                                    resource: path.clone(),
                                    reason: format!("`{}` failed: {stderr}", git_cmd.join(" ")),
                                    suggestion: "Check the repository URL and branch, ensure git is \
                                                 installed in the image, and enable network access \
                                                 with `DockerClient::with_network_mode` — the default \
                                                 mode is `none`."
                                        .to_string(),
                                });
                            }
                        }
                    }
                }
            }
            Ok::<(), SandboxError>(())
        }
        .await;
        if let Err(error) = populated {
            let remove_options = RemoveContainerOptions { force: true, ..Default::default() };
            if let Err(remove_error) =
                self.client.remove_container(&container_id, Some(remove_options)).await
            {
                tracing::warn!(
                    container.id = %container_id,
                    error = %remove_error,
                    "could not remove a container whose provisioning failed"
                );
            }
            return Err(error);
        }

        // Generate session handle and store the mapping
        let session_id = Self::generate_session_id();
        let handle = SessionHandle::new(&session_id);

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id, container_id);

        Ok(handle)
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        let sessions = self.sessions.read().await;
        let container_id = sessions
            .get(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?;

        Ok(Box::new(DockerSession {
            container_id: container_id.clone(),
            client: self.client.clone(),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        let mut sessions = self.sessions.write().await;
        let container_id = sessions
            .remove(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?;
        drop(sessions);

        // Stop the container (with a short grace period)
        let stop_options = StopContainerOptions { t: 5 };
        let _ = self.client.stop_container(&container_id, Some(stop_options)).await;

        // Remove the container
        let remove_options = RemoveContainerOptions { force: true, ..Default::default() };
        self.client.remove_container(&container_id, Some(remove_options)).await.map_err(|e| {
            SandboxError::ExecutionFailed(format!(
                "failed to remove container '{container_id}': {e}"
            ))
        })?;

        Ok(())
    }

    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        let sessions = self.sessions.read().await;
        let container_id = sessions
            .get(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?
            .clone();
        drop(sessions);

        // Generate a unique image tag for the snapshot
        let snapshot_tag = format!("adk-snapshot-{}", uuid::Uuid::new_v4());
        let repo = "adk-sandbox";

        let commit_options = CommitContainerOptions {
            container: container_id.clone(),
            repo: repo.to_string(),
            tag: snapshot_tag.clone(),
            pause: true,
            ..Default::default()
        };

        self.client.commit_container(commit_options, Config::<String>::default()).await.map_err(
            |e| SandboxError::ExecutionFailed(format!("failed to commit container as image: {e}")),
        )?;

        let image_ref = format!("{repo}:{snapshot_tag}");
        Ok(SnapshotId::new(image_ref))
    }

    async fn resume(&self, snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        let image_ref = snapshot_id.as_str();

        // Create a new container from the committed image
        let host_config = self.build_host_config()?;

        let config = container_config(image_ref, self.user.as_deref(), host_config);

        let container = self
            .client
            .create_container(None::<CreateContainerOptions<String>>, config)
            .await
            .map_err(|e| SandboxError::SnapshotNotFound {
                id: format!("failed to create container from snapshot '{image_ref}': {e}"),
            })?;

        let container_id = container.id;

        // Start the container
        self.client.start_container::<String>(&container_id, None).await.map_err(|e| {
            SandboxError::ProvisionFailed {
                resource: image_ref.to_string(),
                reason: format!("failed to start resumed container: {e}"),
                suggestion: "Check Docker daemon status.".to_string(),
            }
        })?;

        // Generate session handle and store the mapping
        let session_id = Self::generate_session_id();
        let handle = SessionHandle::new(&session_id);

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id, container_id);

        Ok(handle)
    }
}

/// A live sandbox session backed by a Docker container.
///
/// Provides workspace operations (exec, read, write, list, patch) against
/// a running Docker container. Commands are executed via `docker exec`
/// with configurable timeouts.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::{DockerClient, Manifest, SandboxClient};
///
/// let client = DockerClient::new().await?;
/// let handle = client.provision(&Manifest { entries: vec![] }).await?;
/// let session = client.start(&handle).await?;
///
/// let output = session.exec_command("echo hello", None).await?;
/// assert_eq!(output.stdout.trim(), "hello");
/// ```
pub struct DockerSession {
    /// The Docker container ID for this session.
    pub container_id: String,
    /// Bollard Docker client for API communication.
    client: Docker,
    /// Maximum duration for individual command executions.
    pub command_timeout: Duration,
}

impl DockerSession {
    /// Executes a command inside the container and captures output as text.
    async fn exec_cmd(
        &self,
        cmd: Vec<&str>,
        working_dir: Option<&str>,
        stdin_content: Option<&[u8]>,
    ) -> Result<(String, String, i64), SandboxError> {
        let (stdout, stderr, exit_code) =
            exec_collect(&self.client, &self.container_id, cmd, working_dir, stdin_content).await?;
        Ok((
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
        ))
    }
}

#[async_trait]
impl SandboxSession for DockerSession {
    async fn exec_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        // Validate working_dir if provided
        let cwd = match working_dir {
            Some(dir) => {
                validate_relative_path(dir)?;
                format!("{CONTAINER_WORKSPACE_ROOT}/{dir}")
            }
            None => CONTAINER_WORKSPACE_ROOT.to_string(),
        };

        let start = std::time::Instant::now();

        let pid_file = format!("/tmp/.adk-exec-{}.pid", uuid::Uuid::new_v4());
        let exec_cmd = timed_exec_command(command, &pid_file);
        let result = tokio::time::timeout(
            self.command_timeout,
            self.exec_cmd(exec_cmd.iter().map(String::as_str).collect(), Some(&cwd), None),
        )
        .await;

        match result {
            Ok(Ok((stdout, stderr, exit_code))) => {
                let duration = start.elapsed();
                Ok(ExecOutput::new(stdout, stderr, exit_code as i32, duration, false))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Dropping the attached stream does not stop a Docker exec, so the command
                // and its descendants are killed from a second exec.
                let kill_cmd = kill_tree_command(&pid_file);
                let killed = tokio::time::timeout(
                    KILL_TIMEOUT,
                    self.exec_cmd(kill_cmd.iter().map(String::as_str).collect(), None, None),
                )
                .await;
                match killed {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => tracing::warn!(
                        container.id = %self.container_id,
                        error = %error,
                        "failed to kill a timed-out command"
                    ),
                    Err(_) => tracing::warn!(
                        container.id = %self.container_id,
                        "killing a timed-out command did not finish"
                    ),
                }
                let duration = start.elapsed();
                Ok(ExecOutput::new("", "", -1, duration, true))
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        validate_relative_path(path)?;
        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

        // Raw bytes: decoding to text would replace every non-UTF-8 byte of a binary file.
        let (stdout, stderr, exit_code) = exec_collect(
            &self.client,
            &self.container_id,
            vec!["cat", "--", &full_path],
            None,
            None,
        )
        .await?;

        if exit_code != 0 {
            let stderr = String::from_utf8_lossy(&stderr);
            if stderr.contains("No such file") {
                return Err(SandboxError::ExecutionFailed(format!("file not found: {path}")));
            }
            return Err(SandboxError::ExecutionFailed(format!(
                "failed to read file '{path}': {stderr}"
            )));
        }

        Ok(stdout)
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxError> {
        validate_relative_path(path)?;
        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

        // Create parent directories
        if let Some(parent_idx) = full_path.rfind('/') {
            let parent = &full_path[..parent_idx];
            let (_, _, code) = self.exec_cmd(vec!["mkdir", "-p", parent], None, None).await?;
            if code != 0 {
                return Err(SandboxError::ExecutionFailed(format!(
                    "failed to create parent directories for '{path}'"
                )));
            }
        }

        // Write content via stdin; the path is an argument, never shell text.
        let write_cmd = write_file_command(&full_path);
        let (_, stderr, exit_code) = self
            .exec_cmd(write_cmd.iter().map(String::as_str).collect(), None, Some(content))
            .await?;

        if exit_code != 0 {
            return Err(SandboxError::ExecutionFailed(format!(
                "failed to write file '{path}': {stderr}"
            )));
        }

        Ok(())
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        validate_relative_path(path)?;
        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

        // Use ls -1F to get entries with type indicators (/ suffix = directory)
        let (stdout, stderr, exit_code) =
            self.exec_cmd(vec!["ls", "-1F", &full_path], None, None).await?;

        if exit_code != 0 {
            if stderr.contains("No such file") || stderr.contains("cannot access") {
                return Err(SandboxError::ExecutionFailed(format!("directory not found: {path}")));
            }
            return Err(SandboxError::ExecutionFailed(format!(
                "failed to list directory '{path}': {stderr}"
            )));
        }

        let entries = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                if let Some(name) = line.strip_suffix('/') {
                    DirEntry::new(name, EntryType::Directory)
                } else {
                    // Strip other type indicators (* for executable, @ for symlink, etc.)
                    let name = line
                        .strip_suffix('*')
                        .or_else(|| line.strip_suffix('@'))
                        .or_else(|| line.strip_suffix('|'))
                        .or_else(|| line.strip_suffix('='))
                        .unwrap_or(line);
                    DirEntry::new(name, EntryType::File)
                }
            })
            .collect();

        Ok(entries)
    }

    async fn apply_patch(&self, patch: &str) -> Result<(), SandboxError> {
        // Apply patch via stdin to the patch command
        let (_, stderr, exit_code) = self
            .exec_cmd(
                vec!["patch", "-p0", "--no-backup-if-mismatch"],
                Some(CONTAINER_WORKSPACE_ROOT),
                Some(patch.as_bytes()),
            )
            .await?;

        if exit_code != 0 {
            return Err(SandboxError::ExecutionFailed(format!("patch failed: {stderr}")));
        }

        Ok(())
    }
}

impl std::fmt::Debug for DockerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerClient")
            .field("base_image", &self.base_image)
            .field("memory_limit_bytes", &self.memory_limit_bytes)
            .field("cpu_limit", &self.cpu_limit)
            .field("user", &self.user)
            .field("bind_mounts", &self.bind_mounts)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for DockerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerSession")
            .field("container_id", &self.container_id)
            .field("command_timeout", &self.command_timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Debug;

    #[test]
    fn debug_does_not_expose_client_internals() {
        // Validates the Debug impl compiles and doesn't include
        // the Docker client field.
        let _: fn(&DockerClient, &mut std::fmt::Formatter<'_>) -> std::fmt::Result =
            <DockerClient as Debug>::fmt;
    }

    #[test]
    fn debug_session_does_not_expose_client() {
        let _: fn(&DockerSession, &mut std::fmt::Formatter<'_>) -> std::fmt::Result =
            <DockerSession as Debug>::fmt;
    }

    #[test]
    fn container_config_runs_as_the_configured_user_in_the_workspace() {
        let host = host_config(None, None, DEFAULT_NETWORK_MODE, None, &[]).unwrap();
        let as_user = container_config("alpine:3.20", Some("1000:1000"), host.clone());
        let as_image = container_config("alpine:3.20", None, host);

        assert_eq!(as_user.user.as_deref(), Some("1000:1000"));
        assert_eq!(as_image.user, None, "without a user the image's default applies");
        for config in [&as_user, &as_image] {
            assert_eq!(config.image.as_deref(), Some("alpine:3.20"));
            assert_eq!(config.working_dir.as_deref(), Some(CONTAINER_WORKSPACE_ROOT));
            assert_eq!(config.cmd, Some(owned(&["sleep", "infinity"])));
        }
    }

    #[test]
    fn container_workspace_root_is_absolute() {
        assert!(CONTAINER_WORKSPACE_ROOT.starts_with('/'));
    }

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn host_config_defaults_drop_capabilities_and_network() {
        let config =
            host_config(None, None, DEFAULT_NETWORK_MODE, Some(DEFAULT_PIDS_LIMIT), &[]).unwrap();

        assert_eq!(config.network_mode.as_deref(), Some("none"));
        assert_eq!(config.cap_drop, Some(owned(&["ALL"])));
        assert_eq!(config.security_opt, Some(owned(&["no-new-privileges"])));
        assert_eq!(config.pids_limit, Some(DEFAULT_PIDS_LIMIT));
        assert_eq!(config.memory, None);
        assert_eq!(config.nano_cpus, None);
        assert_eq!(config.cap_add, None, "no capability may be added back");
        assert_eq!(config.privileged, None);
        assert_eq!(config.mounts, None, "no host path is mounted by default");
        assert_eq!(config.binds, None);
    }

    #[test]
    fn host_config_applies_caller_choices() {
        let config = host_config(Some(512 * 1024 * 1024), Some(1.5), "bridge", None, &[]).unwrap();

        assert_eq!(config.network_mode.as_deref(), Some("bridge"));
        assert_eq!(config.memory, Some(512 * 1024 * 1024));
        assert_eq!(config.nano_cpus, Some(1_500_000_000));
        assert_eq!(config.pids_limit, None);
        assert_eq!(config.cap_drop, Some(owned(&["ALL"])), "capabilities stay dropped");
    }

    #[test]
    fn host_config_saturates_an_out_of_range_memory_limit() {
        let config = host_config(Some(u64::MAX), None, DEFAULT_NETWORK_MODE, None, &[]).unwrap();
        assert_eq!(config.memory, Some(i64::MAX));
    }

    #[test]
    fn host_config_mounts_each_bind_mount_with_its_access() {
        let mounts = [
            BindMount {
                host: PathBuf::from("/home/me/project"),
                container: "/workspace".to_string(),
                read_only: false,
            },
            BindMount {
                host: PathBuf::from("/home/me/a:b"),
                container: "/opt/ref".to_string(),
                read_only: true,
            },
        ];

        let config = host_config(None, None, DEFAULT_NETWORK_MODE, None, &mounts).unwrap();

        assert_eq!(
            config.mounts,
            Some(vec![
                Mount {
                    target: Some("/workspace".to_string()),
                    source: Some("/home/me/project".to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(false),
                    ..Default::default()
                },
                Mount {
                    target: Some("/opt/ref".to_string()),
                    source: Some("/home/me/a:b".to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(true),
                    ..Default::default()
                },
            ])
        );
        assert_eq!(config.binds, None, "mounts are structured, never `host:container` strings");
        assert_eq!(config.cap_drop, Some(owned(&["ALL"])), "capabilities stay dropped");
        assert_eq!(config.security_opt, Some(owned(&["no-new-privileges"])));
    }

    #[cfg(unix)]
    #[test]
    fn host_config_refuses_a_host_path_docker_cannot_express() {
        use std::os::unix::ffi::OsStrExt;

        let mounts = [BindMount {
            host: PathBuf::from(std::ffi::OsStr::from_bytes(b"/home/\xff")),
            container: "/workspace".to_string(),
            read_only: false,
        }];

        let result = host_config(None, None, DEFAULT_NETWORK_MODE, None, &mounts);
        assert!(matches!(result, Err(SandboxError::ProvisionFailed { .. })), "{result:?}");
    }

    #[test]
    fn write_file_command_passes_the_path_as_an_argument() {
        let path = "/workspace/x'; touch /pwned; echo '.txt";
        assert_eq!(
            write_file_command(path),
            owned(&["sh", "-c", r#"cat > "$1""#, "adk-write", path]),
            "the path must stay out of the script text"
        );
    }

    /// Runs the write command with the host's `sh`, which interprets it exactly as the
    /// container's would.
    #[cfg(unix)]
    #[test]
    fn write_file_command_writes_hostile_paths_literally() {
        use std::io::Write;

        let directory = tempfile::tempdir().unwrap();
        let canary = directory.path().join("pwned");
        // Relative, so an injected `touch pwned` would land in the directory under test.
        let name = "x'; touch pwned; echo '$(touch pwned) \"q\".txt";
        let hostile = directory.path().join(name);
        let argv = write_file_command(name);

        let mut child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(directory.path())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"payload").unwrap();
        assert!(child.wait().unwrap().success());

        assert_eq!(std::fs::read(&hostile).unwrap(), b"payload");
        assert!(!canary.exists(), "the path was executed as shell code");
    }

    #[test]
    fn git_clone_commands_pass_url_path_and_branch_as_arguments() {
        let commands =
            git_clone_commands("https://x/'$(id)'.git", Some("main"), "/workspace/r'p").unwrap();

        assert_eq!(
            commands,
            vec![
                owned(&["git", "clone", "--", "https://x/'$(id)'.git", "/workspace/r'p"]),
                owned(&["git", "-C", "/workspace/r'p", "checkout", "main"]),
            ]
        );
        assert_eq!(git_clone_commands("u", None, "/workspace/p").unwrap().len(), 1);
    }

    #[test]
    fn git_clone_commands_reject_a_branch_that_parses_as_an_option() {
        let result = git_clone_commands("u", Some("--upload-pack=touch /pwned"), "/workspace/p");
        assert!(matches!(result, Err(SandboxError::ProvisionFailed { .. })), "{result:?}");
    }

    #[test]
    fn timed_exec_command_passes_the_command_as_an_argument() {
        assert_eq!(
            timed_exec_command("echo 'hi'", "/tmp/p.pid"),
            owned(&["sh", "-c", TIMED_EXEC_SCRIPT, "adk-exec", "echo 'hi'", "/tmp/p.pid"])
        );
        assert_eq!(
            kill_tree_command("/tmp/p.pid"),
            owned(&["sh", "-c", KILL_TREE_SCRIPT, "adk-kill", "/tmp/p.pid"])
        );
    }

    /// The wrapper preserves output and exit status and removes its PID file.
    #[cfg(unix)]
    #[test]
    fn timed_exec_script_reports_the_command_result() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("exec.pid");
        let argv = timed_exec_command("echo out; echo err >&2; exit 7", pid_file.to_str().unwrap());

        let output = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "err\n");
        assert!(!pid_file.exists(), "the PID file must be removed after the command");
    }

    /// The kill script ends the wrapper and the command's background children.
    ///
    /// Containers run each exec as a session leader; the wrapper is placed in its own
    /// process group here to match.
    #[cfg(unix)]
    #[test]
    fn kill_tree_script_kills_the_command_and_its_children() {
        use std::os::unix::process::CommandExt;

        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("exec.pid");
        let marker = directory.path().join("survivor");
        let command = format!("(sleep 1; touch '{}') & sleep 30", marker.display());
        let argv = timed_exec_command(&command, pid_file.to_str().unwrap());

        let mut wrapper =
            std::process::Command::new(&argv[0]).args(&argv[1..]).process_group(0).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !pid_file.exists() {
            assert!(std::time::Instant::now() < deadline, "the wrapper never wrote its PID");
            std::thread::sleep(Duration::from_millis(10));
        }

        let kill = kill_tree_command(pid_file.to_str().unwrap());
        let status = std::process::Command::new(&kill[0]).args(&kill[1..]).status().unwrap();
        assert!(status.success());

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if wrapper.try_wait().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the wrapper survived the kill");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(1_500));
        assert!(!marker.exists(), "a background child survived the kill");
        assert!(!pid_file.exists(), "the kill script must remove the PID file");
    }
}
