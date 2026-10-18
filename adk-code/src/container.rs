//! Container code executor — persistent Docker-backed execution environment.
//!
//! [`DockerExecutor`] manages a persistent Docker container that survives across
//! multiple [`execute`](CodeExecutor::execute) calls. Code is written to files
//! inside the container's working directory and executed via `docker exec`,
//! matching the lifecycle model of AutoGen's `DockerCommandLineCodeExecutor`.
//!
//! # Lifecycle
//!
//! ```text
//! DockerExecutor::new(config)
//!     │
//!     ▼
//!   start()  ──► creates & starts container, runs setup commands
//!     │
//!     ▼
//!   execute() ──► writes code to file, exec's inside running container
//!   execute() ──► reuses same container, accumulates workspace state
//!   execute() ──► ...
//!     │
//!     ▼
//!   stop()   ──► stops & removes container
//! ```
//!
//! # Isolation Model
//!
//! The container's network mode, bind mounts, and environment are fixed by
//! [`DockerConfig`] when it is created. Each request's [`SandboxPolicy`] is
//! checked against them with [`DockerConfig::validate_sandbox_policy`], and a
//! request stricter than the container is rejected before any code runs.
//!
//! | Capability | Enforced | Mechanism |
//! |---|---|---|
//! | Network policy | Yes | `NetworkPolicy::Disabled` requires a container created with network mode `none` |
//! | Filesystem policy | Yes | Every bind mount must lie within the paths the policy grants |
//! | Environment policy | Yes | Container variables must be on the allowlist; allowlisted host variables are passed to the exec |
//! | Timeout | Yes | `tokio::time::timeout` on exec; the timed-out process is killed inside the container |
//! | Privileges | Yes | All capabilities dropped, `no-new-privileges`, a process limit (each can be turned off) |
//! | Output limits | Yes | Output past the request's byte limits is discarded as it streams |
//! | Structured output | Yes | Last-line JSON extraction from stdout |
//! | Persistent workspace | Yes | Container survives across executions |
//! | Interactive sessions | No | Each execute is a separate exec |
//!
//! # Example
//!
//! ```rust,ignore
//! # async fn example() -> Result<(), adk_code::ExecutionError> {
//! use adk_code::{
//!     CodeExecutor, DockerExecutor, DockerConfig,
//!     ExecutionLanguage, ExecutionPayload, ExecutionRequest, SandboxPolicy,
//! };
//!
//! let executor = DockerExecutor::new(DockerConfig::python())?;
//! executor.start().await?;
//!
//! let result = executor.execute(ExecutionRequest {
//!     language: ExecutionLanguage::Python,
//!     payload: ExecutionPayload::Source {
//!         code: "print('hello from persistent container')".to_string(),
//!     },
//!     argv: vec![],
//!     stdin: None,
//!     input: None,
//!     sandbox: SandboxPolicy::strict_rust(),
//!     identity: None,
//! }).await?;
//!
//! // Container is still running — next execute reuses it
//! executor.stop().await?;
//! # Ok(())
//! # }
//! ```

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::host_process;
use crate::{
    BackendCapabilities, CodeExecutor, EnvironmentPolicy, ExecutionError, ExecutionIsolation,
    ExecutionLanguage, ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus,
    FilesystemPolicy, NetworkPolicy, SandboxPolicy, validate_request,
};

/// Configuration for the Docker executor.
///
/// # Example
///
/// ```rust
/// use adk_code::DockerConfig;
///
/// let config = DockerConfig::python();
/// assert_eq!(config.image, "python:3.12-slim");
/// assert_eq!(config.work_dir, "/workspace");
/// ```
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// Container image to use.
    pub image: String,
    /// Working directory inside the container.
    pub work_dir: String,
    /// Setup commands to run after container creation (e.g., `pip install`).
    pub setup_commands: Vec<String>,
    /// Extra environment variables to set in the container.
    pub environment: Vec<String>,
    /// Bind mounts in `host:container[:ro]` format.
    pub bind_mounts: Vec<String>,
    /// Whether to disable network access.
    pub network_disabled: bool,
    /// Container name prefix (a random suffix is appended).
    pub container_name_prefix: String,
    /// Whether to auto-start on first execute if not already running.
    pub auto_start: bool,
    /// Whether to auto-stop and remove the container on drop.
    pub auto_remove: bool,
    /// Drop every Linux capability (`CapDrop: ["ALL"]`). Default: `true`.
    ///
    /// Setup commands that switch users, such as `apt-get`, need it turned off.
    pub drop_capabilities: bool,
    /// Forbid gaining privileges, such as through setuid binaries
    /// (`SecurityOpt: ["no-new-privileges"]`). Default: `true`.
    pub no_new_privileges: bool,
    /// Most processes the container may run (`PidsLimit`), which stops a fork bomb;
    /// `None` sets no limit. Default: [`DEFAULT_PIDS_LIMIT`].
    pub pids_limit: Option<u32>,
}

impl DockerConfig {
    /// Python 3.12 preset.
    pub fn python() -> Self {
        Self {
            image: "python:3.12-slim".to_string(),
            work_dir: "/workspace".to_string(),
            setup_commands: vec![],
            environment: vec![],
            bind_mounts: vec![],
            network_disabled: true,
            container_name_prefix: "adk-python".to_string(),
            auto_start: true,
            auto_remove: true,
            drop_capabilities: true,
            no_new_privileges: true,
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
        }
    }

    /// Node.js 20 preset.
    pub fn node() -> Self {
        Self {
            image: "node:20-slim".to_string(),
            work_dir: "/workspace".to_string(),
            setup_commands: vec![],
            environment: vec![],
            bind_mounts: vec![],
            network_disabled: true,
            container_name_prefix: "adk-node".to_string(),
            auto_start: true,
            auto_remove: true,
            drop_capabilities: true,
            no_new_privileges: true,
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
        }
    }

    /// Custom image preset.
    pub fn custom(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            work_dir: "/workspace".to_string(),
            setup_commands: vec![],
            environment: vec![],
            bind_mounts: vec![],
            network_disabled: true,
            container_name_prefix: "adk-custom".to_string(),
            auto_start: true,
            auto_remove: true,
            drop_capabilities: true,
            no_new_privileges: true,
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
        }
    }

    /// Add a setup command to run after container creation.
    pub fn setup_command(mut self, cmd: impl Into<String>) -> Self {
        self.setup_commands.push(cmd.into());
        self
    }

    /// Add a pip install command for Python dependencies.
    pub fn pip_install(self, packages: &[&str]) -> Self {
        self.setup_command(format!("pip install --quiet {}", packages.join(" ")))
    }

    /// Add an npm install command for Node.js dependencies.
    pub fn npm_install(self, packages: &[&str]) -> Self {
        self.setup_command(format!("npm install --silent {}", packages.join(" ")))
    }

    /// Enable network access (disabled by default).
    pub fn with_network(mut self) -> Self {
        self.network_disabled = false;
        self
    }

    /// Add a bind mount.
    pub fn bind_mount(mut self, mount: impl Into<String>) -> Self {
        self.bind_mounts.push(mount.into());
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, var: impl Into<String>) -> Self {
        self.environment.push(var.into());
        self
    }

    /// Checks that a container created from this configuration satisfies `policy`.
    ///
    /// A persistent container's network mode, bind mounts, and environment are
    /// fixed when it is created, so a request cannot narrow them afterwards.
    /// `DockerExecutor` calls this before every execution and rejects a request
    /// whose policy is stricter than the container:
    ///
    /// | Policy | Satisfied when |
    /// |---|---|
    /// | `NetworkPolicy::Disabled` | `network_disabled` is `true` |
    /// | `FilesystemPolicy::None` | there are no bind mounts |
    /// | `FilesystemPolicy::WorkspaceReadOnly { root }` | every bind mount is read-only (`:ro`) and its host path lies under `root` |
    /// | `FilesystemPolicy::WorkspaceReadWrite { root }` | every bind mount's host path lies under `root` |
    /// | `FilesystemPolicy::Paths { read_only, read_write }` | every bind mount lies under a `read_write` path, or is read-only and lies under a `read_only` path |
    /// | `EnvironmentPolicy::None` | `environment` is empty |
    /// | `EnvironmentPolicy::AllowList(names)` | every `environment` entry's name is in `names` |
    ///
    /// Bind-mount host paths must be absolute and free of `..` components, so a
    /// named volume never satisfies a filesystem policy. Variables defined by the
    /// image itself (`PATH`, `LANG`) come from the image, not the host, and are
    /// not checked.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionError::UnsupportedPolicy`] naming the first setting
    /// that exceeds the policy.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::{DockerConfig, NetworkPolicy, SandboxPolicy};
    ///
    /// let strict = SandboxPolicy::strict_rust();
    /// assert!(DockerConfig::python().validate_sandbox_policy(&strict).is_ok());
    ///
    /// // A networked container cannot honour a request that disables the network.
    /// let networked = DockerConfig::python().with_network();
    /// assert!(networked.validate_sandbox_policy(&strict).is_err());
    ///
    /// let allows_network = SandboxPolicy { network: NetworkPolicy::Enabled, ..strict };
    /// assert!(networked.validate_sandbox_policy(&allows_network).is_ok());
    /// ```
    pub fn validate_sandbox_policy(&self, policy: &SandboxPolicy) -> Result<(), ExecutionError> {
        if matches!(policy.network, NetworkPolicy::Disabled) && !self.network_disabled {
            return Err(ExecutionError::UnsupportedPolicy(
                "the policy disables network access, but the container was created with network \
                 access; drop `with_network()` from the DockerConfig or use \
                 `NetworkPolicy::Enabled`"
                    .to_string(),
            ));
        }

        for mount in &self.bind_mounts {
            // `host:container[:options]`; a Windows drive letter adds a colon to the host part.
            let parts: Vec<&str> = mount.split(':').collect();
            let has_drive_letter = parts.len() > 2
                && parts[0].len() == 1
                && parts[0].chars().all(|c| c.is_ascii_alphabetic());
            let (host, rest) = if has_drive_letter {
                (&mount[..parts[0].len() + 1 + parts[1].len()], &parts[2..])
            } else {
                (parts[0], &parts[1..])
            };
            let read_only = rest
                .get(1)
                .is_some_and(|options| options.split(',').any(|o| o == "ro" || o == "readonly"));

            // `has_root` rather than `is_absolute`: on Windows a Linux-container mount such as
            // `/srv/data` is rooted but not absolute, and containment only needs the path to be
            // anchored and free of `..` for `starts_with` against the policy root to hold.
            let host = Path::new(host);
            let confined = host.has_root()
                && !host.components().any(|component| matches!(component, Component::ParentDir));
            let under = |root: &PathBuf| confined && host.starts_with(root);

            let permitted = match &policy.filesystem {
                FilesystemPolicy::None => false,
                FilesystemPolicy::WorkspaceReadOnly { root } => read_only && under(root),
                FilesystemPolicy::WorkspaceReadWrite { root } => under(root),
                FilesystemPolicy::Paths { read_only: read_only_paths, read_write } => {
                    read_write.iter().any(under) || (read_only && read_only_paths.iter().any(under))
                }
            };
            if !permitted {
                return Err(ExecutionError::UnsupportedPolicy(format!(
                    "bind mount `{mount}` exceeds the filesystem policy {:?}; remove the mount \
                     or grant its host path in the policy",
                    policy.filesystem
                )));
            }
        }

        for entry in &self.environment {
            let name = entry.split_once('=').map_or(entry.as_str(), |(name, _)| name);
            let allowed = match &policy.environment {
                EnvironmentPolicy::None => false,
                EnvironmentPolicy::AllowList(names) => names.iter().any(|allowed| allowed == name),
            };
            if !allowed {
                return Err(ExecutionError::UnsupportedPolicy(format!(
                    "container environment variable `{name}` is not allowed by the environment \
                     policy; remove it from the DockerConfig or add it to \
                     `EnvironmentPolicy::AllowList`"
                )));
            }
        }

        Ok(())
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self::python()
    }
}

// ── Docker SDK implementation (behind `docker` feature) ────────────────

#[cfg(feature = "docker")]
mod docker_impl {
    use super::*;
    use bollard::Docker;
    use bollard::container::{
        Config, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::exec::{CreateExecOptions, StartExecResults};
    use futures::StreamExt;
    use rand::Rng;
    use tokio::sync::RwLock;

    /// Output kept from the executor's own commands (file writes, setup).
    const SETUP_OUTPUT_LIMITS: (usize, usize) = (64 * 1024, 64 * 1024);

    /// State of the managed container.
    #[derive(Debug)]
    struct ContainerState {
        /// Docker container ID.
        id: String,
        /// Whether the container is currently running.
        running: bool,
        /// Counter for generating unique filenames.
        file_counter: u64,
    }

    /// Persistent Docker-backed code execution environment.
    ///
    /// Creates a container once via [`start`](CodeExecutor::start), keeps it running,
    /// and executes code via `docker exec` inside the running container. This matches
    /// the lifecycle model of AutoGen's `DockerCommandLineCodeExecutor`.
    ///
    /// # Sandbox Policy
    ///
    /// The container's network mode, bind mounts, and environment come from
    /// [`DockerConfig`] and cannot change per request. [`execute`](CodeExecutor::execute)
    /// rejects with [`ExecutionError::UnsupportedPolicy`] any request whose policy
    /// the container does not satisfy; see [`DockerConfig::validate_sandbox_policy`].
    /// Variables named by an [`EnvironmentPolicy::AllowList`] are passed from the
    /// host to that request's exec.
    ///
    /// # Cleanup
    ///
    /// Call [`cleanup()`](Self::cleanup) explicitly before dropping to ensure
    /// reliable container removal. The [`Drop`] implementation is best-effort
    /// and requires a tokio runtime to be available.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> Result<(), adk_code::ExecutionError> {
    /// use adk_code::{CodeExecutor, DockerExecutor, DockerConfig};
    ///
    /// let executor = DockerExecutor::new(DockerConfig::python())?;
    /// executor.start().await?;
    /// assert!(executor.is_running().await);
    ///
    /// // Prefer explicit cleanup over relying on Drop
    /// executor.cleanup().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub struct DockerExecutor {
        config: DockerConfig,
        docker: Docker,
        state: RwLock<Option<ContainerState>>,
    }

    impl std::fmt::Debug for DockerExecutor {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("DockerExecutor").field("config", &self.config).finish()
        }
    }

    impl DockerExecutor {
        /// Create a new Docker executor with the given configuration.
        ///
        /// This does NOT start the container — call [`start`](CodeExecutor::start)
        /// or set `auto_start: true` (the default) to have it start on first execute.
        /// # Errors
        ///
        /// Returns [`ExecutionError::InternalError`] if the Docker daemon is
        /// unreachable (not installed, not running, or socket permission denied).
        pub fn new(config: DockerConfig) -> std::result::Result<Self, ExecutionError> {
            let docker = Docker::connect_with_local_defaults().map_err(|e| {
                ExecutionError::InternalError(format!(
                    "failed to connect to Docker daemon: {e}. Is Docker installed and running?"
                ))
            })?;
            Ok(Self { config, docker, state: RwLock::new(None) })
        }

        /// Create with a custom Docker connection.
        pub fn with_docker(config: DockerConfig, docker: Docker) -> Self {
            Self { config, docker, state: RwLock::new(None) }
        }

        /// Explicitly stop and remove the Docker container.
        ///
        /// Prefer calling this method before dropping the executor to ensure
        /// reliable cleanup. The [`Drop`] implementation is best-effort and may
        /// not work if no tokio runtime is available.
        pub async fn cleanup(&self) -> Result<(), ExecutionError> {
            let mut state = self.state.write().await;
            if let Some(s) = state.take() {
                info!(container_id = %s.id, "cleaning up container");
                self.docker
                    .remove_container(
                        &s.id,
                        Some(RemoveContainerOptions { force: true, ..Default::default() }),
                    )
                    .await
                    .map_err(|e| {
                        ExecutionError::ExecutionFailed(format!("failed to remove container: {e}"))
                    })?;
            }
            Ok(())
        }

        /// The container's network, mounts and privilege limits.
        fn host_config(&self) -> bollard::models::HostConfig {
            let config = &self.config;
            bollard::models::HostConfig {
                network_mode: config.network_disabled.then(|| "none".to_string()),
                binds: (!config.bind_mounts.is_empty()).then(|| config.bind_mounts.clone()),
                cap_drop: config.drop_capabilities.then(|| vec!["ALL".to_string()]),
                security_opt: config
                    .no_new_privileges
                    .then(|| vec!["no-new-privileges".to_string()]),
                pids_limit: config.pids_limit.map(i64::from),
                ..Default::default()
            }
        }

        /// Generate a unique container name.
        fn container_name(&self) -> String {
            let suffix: u32 = rand::rng().random_range(100_000..999_999);
            format!("{}-{suffix}", self.config.container_name_prefix)
        }

        /// Get the file extension for a language.
        fn file_extension(lang: &ExecutionLanguage) -> &'static str {
            match lang {
                ExecutionLanguage::Python => "py",
                ExecutionLanguage::JavaScript => "js",
                ExecutionLanguage::Rust => "rs",
                ExecutionLanguage::Command => "sh",
                ExecutionLanguage::Wasm => "wasm",
            }
        }

        /// Get the execution command for a language and filename.
        fn exec_command(lang: &ExecutionLanguage, filename: &str) -> Vec<String> {
            match lang {
                ExecutionLanguage::Python => {
                    vec!["python3".to_string(), filename.to_string()]
                }
                ExecutionLanguage::JavaScript => {
                    vec!["node".to_string(), filename.to_string()]
                }
                ExecutionLanguage::Command => {
                    vec!["sh".to_string(), filename.to_string()]
                }
                _ => vec![],
            }
        }

        /// Write a file inside the running container.
        async fn write_file(
            &self,
            container_id: &str,
            path: &str,
            content: &str,
        ) -> Result<(), ExecutionError> {
            // Use a heredoc-style approach to write file content safely.
            // Base64 encode to avoid shell escaping issues.
            let encoded = base64_encode(content.as_bytes());
            let cmd = vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo '{encoded}' | base64 -d > {path}"),
            ];
            self.exec_in_container(container_id, &cmd, None, None, SETUP_OUTPUT_LIMITS).await?;
            Ok(())
        }

        /// Execute a command inside the running container and capture output.
        ///
        /// `env` entries (`KEY=VALUE`) are added to the container's own environment
        /// for this exec only. Output past `limits` (stdout, stderr bytes) is discarded
        /// while the stream is drained. When `timeout` passes, the process is killed
        /// inside the container: ending the exec stream alone leaves it running.
        async fn exec_in_container(
            &self,
            container_id: &str,
            cmd: &[String],
            env: Option<Vec<String>>,
            timeout: Option<std::time::Duration>,
            limits: (usize, usize),
        ) -> Result<ExecOutput, ExecutionError> {
            let pid_file =
                timeout.map(|_| format!("/tmp/.adk-exec-{}.pid", super::container_name("pid")));
            let cmd = match &pid_file {
                Some(pid_file) => with_pid_file(pid_file, cmd),
                None => cmd.to_vec(),
            };
            let exec_output = self.run_exec(container_id, cmd, env, limits);

            match (timeout, pid_file) {
                (Some(dur), Some(pid_file)) => match tokio::time::timeout(dur, exec_output).await {
                    Ok(result) => result,
                    Err(_) => {
                        self.kill_exec(container_id, &pid_file).await;
                        Err(ExecutionError::Timeout(dur.as_millis() as u64))
                    }
                },
                _ => exec_output.await,
            }
        }

        /// Runs one exec to completion and captures its output within `limits`.
        async fn run_exec(
            &self,
            container_id: &str,
            cmd: Vec<String>,
            env: Option<Vec<String>>,
            limits: (usize, usize),
        ) -> Result<ExecOutput, ExecutionError> {
            let exec = self
                .docker
                .create_exec(
                    container_id,
                    CreateExecOptions {
                        cmd: Some(cmd),
                        env,
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        working_dir: Some(self.config.work_dir.clone()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| {
                    ExecutionError::ExecutionFailed(format!("failed to create exec: {e}"))
                })?;

            {
                match self.docker.start_exec(&exec.id, None).await {
                    Ok(StartExecResults::Attached { mut output, .. }) => {
                        let mut captured = ExecOutput::default();

                        while let Some(chunk) = output.next().await {
                            match chunk {
                                Ok(bollard::container::LogOutput::StdOut { message }) => {
                                    push_bounded(
                                        &mut captured.stdout,
                                        &mut captured.stdout_truncated,
                                        &message,
                                        limits.0,
                                    );
                                }
                                Ok(bollard::container::LogOutput::StdErr { message }) => {
                                    push_bounded(
                                        &mut captured.stderr,
                                        &mut captured.stderr_truncated,
                                        &message,
                                        limits.1,
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    return Err(ExecutionError::ExecutionFailed(format!(
                                        "exec stream error: {e}"
                                    )));
                                }
                            }
                        }

                        // Get exit code.
                        let inspect = self.docker.inspect_exec(&exec.id).await.map_err(|e| {
                            ExecutionError::ExecutionFailed(format!("failed to inspect exec: {e}"))
                        })?;
                        captured.exit_code = inspect.exit_code;

                        Ok(captured)
                    }
                    Ok(StartExecResults::Detached) => Ok(ExecOutput::default()),
                    Err(e) => {
                        Err(ExecutionError::ExecutionFailed(format!("failed to start exec: {e}")))
                    }
                }
            }
        }

        /// Kills the process group, or failing that the process, recorded in `pid_file`.
        async fn kill_exec(&self, container_id: &str, pid_file: &str) {
            let kill = vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"pid=$(cat "$0") && { kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid"; }"#
                    .to_string(),
                pid_file.to_string(),
            ];
            let killed = tokio::time::timeout(
                super::KILL_TIMEOUT,
                self.run_exec(container_id, kill, None, (0, 4096)),
            )
            .await;
            match killed {
                Ok(Ok(output)) if output.exit_code == Some(0) => {}
                Ok(Ok(output)) => warn!(
                    container_id = %container_id,
                    exit_code = ?output.exit_code,
                    stderr = %output.stderr,
                    "killing the timed-out exec failed"
                ),
                Ok(Err(error)) => {
                    warn!(container_id = %container_id, %error, "killing the timed-out exec failed")
                }
                Err(_) => {
                    warn!(container_id = %container_id, "killing the timed-out exec did not finish")
                }
            }
        }
    }

    /// What one exec printed, within the request's limits.
    #[derive(Debug, Default)]
    struct ExecOutput {
        stdout: String,
        stderr: String,
        exit_code: Option<i64>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    }

    /// `cmd` run through a shell that first writes its PID to `pid_file`; `exec` keeps
    /// the PID for `cmd` itself.
    fn with_pid_file(pid_file: &str, cmd: &[String]) -> Vec<String> {
        let mut wrapped = vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"echo $$ > "$0" && exec "$@""#.to_string(),
            pid_file.to_string(),
        ];
        wrapped.extend(cmd.iter().cloned());
        wrapped
    }

    /// Appends `chunk` to `buffer` without taking it past `limit` bytes.
    fn push_bounded(buffer: &mut String, truncated: &mut bool, chunk: &[u8], limit: usize) {
        if *truncated {
            return;
        }
        let text = String::from_utf8_lossy(chunk);
        let room = limit.saturating_sub(buffer.len());
        if text.len() <= room {
            buffer.push_str(&text);
        } else {
            buffer.push_str(&text[..text.floor_char_boundary(room)]);
            *truncated = true;
        }
    }

    #[async_trait]
    impl CodeExecutor for DockerExecutor {
        fn name(&self) -> &str {
            "docker"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                isolation: ExecutionIsolation::ContainerPersistent,
                enforce_network_policy: true,
                enforce_filesystem_policy: true,
                enforce_environment_policy: true,
                enforce_timeout: true,
                supports_structured_output: true,
                supports_process_execution: true,
                supports_persistent_workspace: true,
                supports_interactive_sessions: false,
            }
        }

        fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
            matches!(
                lang,
                ExecutionLanguage::Python
                    | ExecutionLanguage::JavaScript
                    | ExecutionLanguage::Command
            )
        }

        async fn start(&self) -> Result<(), ExecutionError> {
            let mut state = self.state.write().await;
            if state.as_ref().is_some_and(|s| s.running) {
                return Ok(());
            }

            let name = self.container_name();
            info!(image = %self.config.image, container = %name, "creating container");

            let host_config = self.host_config();

            let env = if self.config.environment.is_empty() {
                None
            } else {
                Some(self.config.environment.clone())
            };

            let container_config = Config {
                image: Some(self.config.image.clone()),
                working_dir: Some(self.config.work_dir.clone()),
                env,
                host_config: Some(host_config),
                // Keep container alive with a long sleep.
                cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
                tty: Some(false),
                ..Default::default()
            };

            let create_opts = CreateContainerOptions { name: name.clone(), ..Default::default() };

            let response =
                self.docker.create_container(Some(create_opts), container_config).await.map_err(
                    |e| ExecutionError::ExecutionFailed(format!("failed to create container: {e}")),
                )?;

            let container_id = response.id;
            debug!(container_id = %container_id, "container created");

            // Start the container.
            self.docker
                .start_container(&container_id, None::<StartContainerOptions<String>>)
                .await
                .map_err(|e| {
                    ExecutionError::ExecutionFailed(format!("failed to start container: {e}"))
                })?;

            info!(container_id = %container_id, "container started");

            // Create workspace directory.
            let mkdir_cmd =
                vec!["mkdir".to_string(), "-p".to_string(), self.config.work_dir.clone()];
            let _ = self
                .exec_in_container(&container_id, &mkdir_cmd, None, None, SETUP_OUTPUT_LIMITS)
                .await;

            // Run setup commands.
            for setup_cmd in &self.config.setup_commands {
                info!(cmd = %setup_cmd, "running setup command");
                let cmd = vec!["sh".to_string(), "-c".to_string(), setup_cmd.clone()];
                let ExecOutput { stderr, exit_code, .. } = self
                    .exec_in_container(&container_id, &cmd, None, None, SETUP_OUTPUT_LIMITS)
                    .await?;

                if exit_code != Some(0) {
                    warn!(
                        exit_code = ?exit_code,
                        stderr = %stderr,
                        "setup command failed"
                    );
                    // Clean up on setup failure.
                    let _ = self
                        .docker
                        .remove_container(
                            &container_id,
                            Some(RemoveContainerOptions { force: true, ..Default::default() }),
                        )
                        .await;
                    return Err(ExecutionError::ExecutionFailed(format!(
                        "setup command failed: {setup_cmd}\nstderr: {stderr}"
                    )));
                }
            }

            *state = Some(ContainerState { id: container_id, running: true, file_counter: 0 });

            Ok(())
        }

        async fn stop(&self) -> Result<(), ExecutionError> {
            let mut state = self.state.write().await;
            if let Some(s) = state.take() {
                info!(container_id = %s.id, "stopping container");
                let _ = self
                    .docker
                    .remove_container(
                        &s.id,
                        Some(RemoveContainerOptions { force: true, ..Default::default() }),
                    )
                    .await;
            }
            Ok(())
        }

        async fn is_running(&self) -> bool {
            self.state.read().await.as_ref().is_some_and(|s| s.running)
        }

        async fn execute(
            &self,
            request: ExecutionRequest,
        ) -> Result<ExecutionResult, ExecutionError> {
            let supported = [
                ExecutionLanguage::Python,
                ExecutionLanguage::JavaScript,
                ExecutionLanguage::Command,
            ];
            validate_request(&self.capabilities(), &supported, &request)?;
            self.config.validate_sandbox_policy(&request.sandbox)?;

            let code = match &request.payload {
                ExecutionPayload::Source { code } if code.trim().is_empty() => {
                    return Err(ExecutionError::InvalidRequest("empty source code".to_string()));
                }
                ExecutionPayload::Source { code } => code.clone(),
                ExecutionPayload::GuestModule { .. } => {
                    return Err(ExecutionError::InvalidRequest(
                        "DockerExecutor does not support guest modules".to_string(),
                    ));
                }
            };

            // Auto-start if configured and not running.
            if self.config.auto_start && !self.is_running().await {
                self.start().await?;
            }

            // Get container ID and increment file counter.
            let (container_id, filename) = {
                let mut state = self.state.write().await;
                let s = state.as_mut().ok_or_else(|| {
                    ExecutionError::ExecutionFailed(
                        "container not started — call start() first".to_string(),
                    )
                })?;
                s.file_counter += 1;
                let ext = Self::file_extension(&request.language);
                let filename = format!("{}/code_{}.{ext}", self.config.work_dir, s.file_counter);
                (s.id.clone(), filename)
            };

            let start = Instant::now();

            // Write code to file inside container.
            self.write_file(&container_id, &filename, &code).await?;

            // If there's structured input, write it as a JSON file.
            if let Some(ref input) = request.input {
                let input_json = serde_json::to_string(input).unwrap_or_default();
                let input_path = format!("{}/input.json", self.config.work_dir);
                self.write_file(&container_id, &input_path, &input_json).await?;
            }

            // Build execution command.
            let exec_cmd = Self::exec_command(&request.language, &filename);
            if exec_cmd.is_empty() {
                return Err(ExecutionError::UnsupportedLanguage(format!("{}", request.language)));
            }

            debug!(
                container_id = %container_id,
                language = %request.language,
                filename = %filename,
                "executing code in container"
            );

            // Mirrors `docker run --env NAME`: allowlisted variables carry the host's values.
            let exec_env = host_process::allowlisted_host_env(&request.sandbox.environment)
                .into_iter()
                .map(|(name, value)| format!("{name}={}", value.to_string_lossy()))
                .collect::<Vec<_>>();

            // Execute with timeout.
            let ExecOutput { stdout, stderr, exit_code, stdout_truncated, stderr_truncated } = self
                .exec_in_container(
                    &container_id,
                    &exec_cmd,
                    Some(exec_env),
                    Some(request.sandbox.timeout),
                    (request.sandbox.max_stdout_bytes, request.sandbox.max_stderr_bytes),
                )
                .await?;

            let duration_ms = start.elapsed().as_millis() as u64;

            let (structured_output, display_stdout) = extract_structured_output(&stdout);

            let status = match exit_code {
                Some(0) => ExecutionStatus::Success,
                _ => ExecutionStatus::Failed,
            };

            info!(
                exit_code = ?exit_code,
                duration_ms,
                has_structured_output = structured_output.is_some(),
                "container execution completed"
            );

            Ok(ExecutionResult {
                status,
                stdout: display_stdout,
                stderr,
                output: structured_output,
                exit_code: exit_code.map(|c| c as i32),
                stdout_truncated,
                stderr_truncated,
                duration_ms,
                metadata: None,
            })
        }
    }

    impl Drop for DockerExecutor {
        fn drop(&mut self) {
            if self.config.auto_remove {
                // Best-effort cleanup — we can't await in drop, so spawn a task
                // only if a tokio runtime is available.
                if let Some(state) = self.state.get_mut().take() {
                    let docker = self.docker.clone();
                    let container_id = state.id;
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            handle.spawn(async move {
                                let _ = docker
                                    .remove_container(
                                        &container_id,
                                        Some(RemoveContainerOptions {
                                            force: true,
                                            ..Default::default()
                                        }),
                                    )
                                    .await;
                            });
                        }
                        Err(_) => {
                            tracing::warn!(
                                container_id = %container_id,
                                "no tokio runtime available during DockerExecutor drop, \
                                 container may leak. Call cleanup() explicitly before dropping."
                            );
                        }
                    }
                }
            }
        }
    }

    /// Simple base64 encoder (no padding issues with shell).
    fn base64_encode(data: &[u8]) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
            result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                result.push('=');
            }
            if chunk.len() > 2 {
                result.push(CHARS[(triple & 0x3F) as usize] as char);
            } else {
                result.push('=');
            }
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_container_drops_privileges_and_limits_processes_unless_opted_out() {
            let docker = Docker::connect_with_local_defaults().expect("client");
            let hardened = DockerExecutor::with_docker(DockerConfig::python(), docker.clone());
            let host = hardened.host_config();
            assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
            assert_eq!(host.security_opt, Some(vec!["no-new-privileges".to_string()]));
            assert_eq!(host.pids_limit, Some(i64::from(DEFAULT_PIDS_LIMIT)));
            assert_eq!(host.network_mode.as_deref(), Some("none"));

            let config = DockerConfig {
                drop_capabilities: false,
                no_new_privileges: false,
                pids_limit: None,
                ..DockerConfig::python()
            };
            let host = DockerExecutor::with_docker(config, docker).host_config();
            assert_eq!((host.cap_drop, host.security_opt, host.pids_limit), (None, None, None));
        }

        #[test]
        fn exec_commands_record_their_pid_and_keep_it() {
            let cmd = with_pid_file("/tmp/x.pid", &["python3".to_string(), "a.py".to_string()]);
            assert_eq!(
                cmd,
                ["sh", "-c", r#"echo $$ > "$0" && exec "$@""#, "/tmp/x.pid", "python3", "a.py"]
            );
        }

        #[test]
        fn output_past_the_limit_is_discarded_as_it_arrives() {
            let (mut buffer, mut truncated) = (String::new(), false);
            push_bounded(&mut buffer, &mut truncated, b"abc", 5);
            assert_eq!((buffer.as_str(), truncated), ("abc", false));
            push_bounded(&mut buffer, &mut truncated, "déf".as_bytes(), 5);
            // "é" would cross the limit, so the cut falls before it.
            assert_eq!((buffer.as_str(), truncated), ("abcd", true));
            push_bounded(&mut buffer, &mut truncated, b"more", 5);
            assert_eq!(buffer, "abcd");
        }
    }
}

#[cfg(feature = "docker")]
pub use docker_impl::DockerExecutor;

// ── CLI-based fallback (always available) ──────────────────────────────

/// Configuration for the CLI-based container command executor.
///
/// This is the fallback executor that shells out to `docker run` for each
/// execution. For production use, prefer `DockerExecutor` (behind the
/// `docker` feature) which uses persistent containers.
///
/// # Example
///
/// ```rust
/// use adk_code::ContainerConfig;
///
/// let config = ContainerConfig::default();
/// assert_eq!(config.runtime, "docker");
/// ```
#[derive(Debug, Clone)]
pub struct ContainerConfig {
    /// Container runtime binary (e.g., `"docker"`, `"podman"`).
    pub runtime: String,
    /// Default container image when not overridden per-request.
    pub default_image: String,
    /// Extra flags passed to the container runtime `run` command.
    pub extra_flags: Vec<String>,
    /// Whether to automatically remove the container after execution.
    pub auto_remove: bool,
    /// Drop every Linux capability (`--cap-drop=ALL`). Default: `true`.
    pub drop_capabilities: bool,
    /// Forbid gaining privileges, such as through setuid binaries
    /// (`--security-opt=no-new-privileges`). Default: `true`.
    pub no_new_privileges: bool,
    /// Most processes the container may run (`--pids-limit`), which stops a fork bomb;
    /// `None` sets no limit. Default: [`DEFAULT_PIDS_LIMIT`].
    pub pids_limit: Option<u32>,
}

/// Default process limit for containers the executors create.
pub const DEFAULT_PIDS_LIMIT: u32 = 256;

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            runtime: "docker".to_string(),
            default_image: "python:3.12-slim".to_string(),
            extra_flags: vec![],
            auto_remove: true,
            drop_capabilities: true,
            no_new_privileges: true,
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
        }
    }
}

/// CLI-based container executor that shells out to `docker run` per execution.
///
/// Each [`execute`](CodeExecutor::execute) call spawns a new ephemeral container.
/// This is simpler but less efficient than `DockerExecutor` which reuses a
/// persistent container.
///
/// Each container is named, so it can be stopped: when the request's timeout passes,
/// or the call is dropped, the executor runs `{runtime} kill {name}`, because ending
/// the `docker run` client process leaves the container running. Containers run with
/// every capability dropped, `no-new-privileges` and a process limit unless
/// [`ContainerConfig`] turns them off.
///
/// For production use, prefer `DockerExecutor` (behind the `docker` feature).
///
/// # Example
///
/// ```rust
/// use adk_code::{CodeExecutor, ContainerCommandExecutor, ContainerConfig, ExecutionIsolation};
///
/// let executor = ContainerCommandExecutor::default();
/// assert_eq!(executor.name(), "container-command");
/// assert_eq!(executor.capabilities().isolation, ExecutionIsolation::ContainerEphemeral);
/// ```
#[derive(Debug, Clone)]
pub struct ContainerCommandExecutor {
    config: ContainerConfig,
}

impl ContainerCommandExecutor {
    /// Create a new container command executor with the given configuration.
    pub fn new(config: ContainerConfig) -> Self {
        Self { config }
    }

    /// Build the container `run` command arguments for a given request.
    fn build_run_args(&self, request: &ExecutionRequest, name: &str) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        if self.config.auto_remove {
            args.push("--rm".to_string());
        }

        args.push("-i".to_string());
        args.push("--name".to_string());
        args.push(name.to_string());
        if self.config.drop_capabilities {
            args.push("--cap-drop=ALL".to_string());
        }
        if self.config.no_new_privileges {
            args.push("--security-opt=no-new-privileges".to_string());
        }
        if let Some(limit) = self.config.pids_limit {
            args.push(format!("--pids-limit={limit}"));
        }

        match request.sandbox.network {
            NetworkPolicy::Disabled => {
                args.push("--network=none".to_string());
            }
            NetworkPolicy::Enabled => {}
        }

        match &request.sandbox.filesystem {
            FilesystemPolicy::None => {}
            FilesystemPolicy::WorkspaceReadOnly { root } => {
                args.push("-v".to_string());
                args.push(format!("{}:/workspace:ro", root.display()));
            }
            FilesystemPolicy::WorkspaceReadWrite { root } => {
                args.push("-v".to_string());
                args.push(format!("{}:/workspace:rw", root.display()));
            }
            FilesystemPolicy::Paths { read_only, read_write } => {
                for path in read_only {
                    args.push("-v".to_string());
                    args.push(format!("{}:{}:ro", path.display(), path.display()));
                }
                for path in read_write {
                    args.push("-v".to_string());
                    args.push(format!("{}:{}:rw", path.display(), path.display()));
                }
            }
        }

        if let EnvironmentPolicy::AllowList(vars) = &request.sandbox.environment {
            for var in vars {
                args.push("--env".to_string());
                args.push(var.clone());
            }
        }

        if let Some(ref wd) = request.sandbox.working_directory {
            args.push("-w".to_string());
            args.push(wd.display().to_string());
        }

        args.extend(self.config.extra_flags.clone());
        args.push(self.config.default_image.clone());

        let code = match &request.payload {
            ExecutionPayload::Source { code } => code.clone(),
            ExecutionPayload::GuestModule { .. } => String::new(),
        };

        match request.language {
            ExecutionLanguage::Python => {
                args.push("python3".to_string());
                args.push("-c".to_string());
                args.push(code);
            }
            ExecutionLanguage::JavaScript => {
                args.push("node".to_string());
                args.push("-e".to_string());
                args.push(code);
            }
            ExecutionLanguage::Command => {
                args.push("sh".to_string());
                args.push("-c".to_string());
                args.push(code);
            }
            _ => {}
        }

        args.extend(request.argv.clone());
        args
    }
}

impl Default for ContainerCommandExecutor {
    fn default() -> Self {
        Self::new(ContainerConfig::default())
    }
}

#[async_trait]
impl CodeExecutor for ContainerCommandExecutor {
    fn name(&self) -> &str {
        "container-command"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            isolation: ExecutionIsolation::ContainerEphemeral,
            enforce_network_policy: true,
            enforce_filesystem_policy: true,
            enforce_environment_policy: true,
            enforce_timeout: true,
            supports_structured_output: true,
            supports_process_execution: true,
            supports_persistent_workspace: false,
            supports_interactive_sessions: false,
        }
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        matches!(
            lang,
            ExecutionLanguage::Python | ExecutionLanguage::JavaScript | ExecutionLanguage::Command
        )
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let supported =
            [ExecutionLanguage::Python, ExecutionLanguage::JavaScript, ExecutionLanguage::Command];
        validate_request(&self.capabilities(), &supported, &request)?;

        match &request.payload {
            ExecutionPayload::Source { code } if code.trim().is_empty() => {
                return Err(ExecutionError::InvalidRequest("empty source code".to_string()));
            }
            ExecutionPayload::Source { .. } => {}
            ExecutionPayload::GuestModule { .. } => {
                return Err(ExecutionError::InvalidRequest(
                    "ContainerCommandExecutor does not support guest modules".to_string(),
                ));
            }
        }

        let start = Instant::now();
        let name = container_name("adk-exec");
        let run_args = self.build_run_args(&request, &name);

        debug!(
            runtime = %self.config.runtime,
            image = %self.config.default_image,
            language = %request.language,
            "starting container execution"
        );

        // Structured input takes precedence over raw stdin.
        let stdin = match (&request.input, &request.stdin) {
            (Some(input), _) => Some(serde_json::to_vec(input).unwrap_or_default()),
            (None, Some(raw)) => Some(raw.clone()),
            (None, None) => None,
        };

        let mut cmd = tokio::process::Command::new(&self.config.runtime);
        cmd.args(&run_args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| {
            ExecutionError::ExecutionFailed(format!(
                "failed to spawn container runtime '{}': {e}",
                self.config.runtime
            ))
        })?;
        // Killing the `run` client process does not stop its container.
        let mut container = RunningContainer::new(&self.config.runtime, &name);

        let completed = host_process::run_to_completion(
            child,
            stdin,
            request.sandbox.timeout,
            request.sandbox.max_stdout_bytes,
            request.sandbox.max_stderr_bytes,
        )
        .await
        .map_err(|e| {
            ExecutionError::ExecutionFailed(format!("failed to wait for container: {e}"))
        })?;

        let Some(output) = completed else {
            warn!(container.name = %name, "container execution timed out; killing the container");
            container.kill().await;
            return Ok(ExecutionResult {
                status: ExecutionStatus::Timeout,
                stdout: String::new(),
                stderr: String::new(),
                output: None,
                exit_code: None,
                stdout_truncated: false,
                stderr_truncated: false,
                duration_ms: start.elapsed().as_millis() as u64,
                metadata: None,
            });
        };

        container.finished();
        let duration_ms = start.elapsed().as_millis() as u64;

        let raw_stdout = String::from_utf8_lossy(&output.stdout.bytes).into_owned();
        let raw_stderr = String::from_utf8_lossy(&output.stderr.bytes).into_owned();

        let (stdout, stdout_truncated) =
            truncate_output(raw_stdout, request.sandbox.max_stdout_bytes);
        let (stderr, stderr_truncated) =
            truncate_output(raw_stderr, request.sandbox.max_stderr_bytes);
        let stdout_truncated = stdout_truncated || output.stdout.truncated;
        let stderr_truncated = stderr_truncated || output.stderr.truncated;

        let (structured_output, display_stdout) = extract_structured_output(&stdout);

        let status = if output.status.success() {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::Failed
        };

        info!(
            exit_code = output.status.code(),
            duration_ms,
            has_structured_output = structured_output.is_some(),
            "container execution completed"
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
}

// ── Shared helpers ─────────────────────────────────────────────────────

/// How long the executors wait for `kill` to stop a container that outlived its timeout.
const KILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A container name unique to this process: `{prefix}-{pid}-{nanos}-{counter}`.
fn container_name(prefix: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let counter = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}-{}-{nanos}-{counter}", std::process::id())
}

/// A container started by `{runtime} run`, killed unless it is marked finished.
struct RunningContainer {
    runtime: String,
    name: String,
    running: bool,
}

impl RunningContainer {
    fn new(runtime: &str, name: &str) -> Self {
        Self { runtime: runtime.to_string(), name: name.to_string(), running: true }
    }

    /// The container exited on its own.
    fn finished(&mut self) {
        self.running = false;
    }

    /// Runs `{runtime} kill {name}` and waits for it.
    async fn kill(&mut self) {
        self.running = false;
        let kill = tokio::process::Command::new(&self.runtime)
            .args(["kill", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        match tokio::time::timeout(KILL_TIMEOUT, kill).await {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => warn!(container.name = %self.name, ?status, "container kill failed"),
            Ok(Err(error)) => warn!(container.name = %self.name, %error, "container kill failed"),
            Err(_) => warn!(container.name = %self.name, "container kill did not finish"),
        }
    }
}

impl Drop for RunningContainer {
    fn drop(&mut self) {
        // The call was dropped mid-run; the container would otherwise keep running.
        if self.running {
            let mut kill = std::process::Command::new(&self.runtime);
            kill.args(["kill", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // A thread, so the kill is reaped without blocking the dropping task.
            std::thread::spawn(move || {
                let _ = kill.status();
            });
        }
    }
}

/// Truncate output to the given byte limit.
fn truncate_output(output: String, max_bytes: usize) -> (String, bool) {
    if output.len() <= max_bytes {
        (output, false)
    } else {
        let truncated = output
            .char_indices()
            .take_while(|(i, _)| *i < max_bytes)
            .map(|(_, c)| c)
            .collect::<String>();
        (truncated, true)
    }
}

/// Extract structured JSON output from the last line of stdout.
fn extract_structured_output(stdout: &str) -> (Option<serde_json::Value>, String) {
    let trimmed = stdout.trim_end();
    if trimmed.is_empty() {
        return (None, String::new());
    }

    if let Some(last_newline_pos) = trimmed.rfind('\n') {
        let last_line = &trimmed[last_newline_pos + 1..];
        let before = &trimmed[..last_newline_pos];

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(last_line) {
            return (Some(value), before.to_string());
        }
    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return (Some(value), String::new());
    }

    (None, stdout.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_container_ephemeral() {
        let executor = ContainerCommandExecutor::default();
        let caps = executor.capabilities();
        assert_eq!(caps.isolation, ExecutionIsolation::ContainerEphemeral);
        assert!(caps.enforce_network_policy);
        assert!(caps.enforce_filesystem_policy);
        assert!(caps.enforce_environment_policy);
        assert!(caps.enforce_timeout);
        assert!(caps.supports_structured_output);
        assert!(caps.supports_process_execution);
        assert!(!caps.supports_persistent_workspace);
        assert!(!caps.supports_interactive_sessions);
    }

    #[test]
    fn supports_python_js_command() {
        let executor = ContainerCommandExecutor::default();
        assert!(executor.supports_language(&ExecutionLanguage::Python));
        assert!(executor.supports_language(&ExecutionLanguage::JavaScript));
        assert!(executor.supports_language(&ExecutionLanguage::Command));
        assert!(!executor.supports_language(&ExecutionLanguage::Rust));
        assert!(!executor.supports_language(&ExecutionLanguage::Wasm));
    }

    #[test]
    fn default_config() {
        let config = ContainerConfig::default();
        assert_eq!(config.runtime, "docker");
        assert_eq!(config.default_image, "python:3.12-slim");
        assert!(config.extra_flags.is_empty());
        assert!(config.auto_remove);
    }

    #[test]
    fn build_run_args_basic_python() {
        let executor = ContainerCommandExecutor::default();
        let request = ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::Source { code: "print('hello')".to_string() },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox: crate::SandboxPolicy::strict_rust(),
            identity: None,
        };

        let args = executor.build_run_args(&request, "adk-exec-test");
        assert!(args.contains(&"run".to_string()));
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"--network=none".to_string()));
        assert!(args.contains(&"python3".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"print('hello')".to_string()));
    }

    #[test]
    fn build_run_args_with_network_enabled() {
        let executor = ContainerCommandExecutor::default();
        let mut sandbox = crate::SandboxPolicy::strict_rust();
        sandbox.network = NetworkPolicy::Enabled;

        let request = ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::Source { code: "print('hello')".to_string() },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox,
            identity: None,
        };

        let args = executor.build_run_args(&request, "adk-exec-test");
        assert!(!args.contains(&"--network=none".to_string()));
    }

    fn python_request(code: &str, timeout: std::time::Duration) -> ExecutionRequest {
        let mut sandbox = crate::SandboxPolicy::strict_rust();
        sandbox.timeout = timeout;
        ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::Source { code: code.to_string() },
            argv: vec![],
            stdin: None,
            input: None,
            sandbox,
            identity: None,
        }
    }

    #[test]
    fn build_run_args_names_and_hardens_the_container_unless_opted_out() {
        let request = python_request("print(1)", std::time::Duration::from_secs(5));
        let args = ContainerCommandExecutor::default().build_run_args(&request, "adk-exec-test");
        let name_at = args.iter().position(|arg| arg == "--name").expect("--name");
        assert_eq!(args[name_at + 1], "adk-exec-test");
        for flag in ["--cap-drop=ALL", "--security-opt=no-new-privileges", "--pids-limit=256"] {
            assert!(args.contains(&flag.to_string()), "{flag} missing from {args:?}");
        }

        let open = ContainerCommandExecutor::new(ContainerConfig {
            drop_capabilities: false,
            no_new_privileges: false,
            pids_limit: None,
            ..ContainerConfig::default()
        });
        let args = open.build_run_args(&request, "adk-exec-test");
        assert!(!args.iter().any(|arg| arg.starts_with("--cap-drop")
            || arg.starts_with("--security-opt")
            || arg.starts_with("--pids-limit")));
    }

    /// A stand-in container runtime that logs its arguments; `run` blocks like a
    /// container whose code never ends.
    #[cfg(unix)]
    fn logging_runtime(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("runtime.log");
        let runtime = dir.join("runtime");
        std::fs::write(
            &runtime,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1\" in run) exec sleep 30 ;; esac\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The first run of a new executable can be delayed by the OS (macOS scans it).
        std::process::Command::new(&runtime).arg("warm-up").status().unwrap();
        (runtime, log)
    }

    /// The name given to `run`, and whether a later invocation killed it.
    #[cfg(unix)]
    fn killed_container(log: &std::path::Path) -> (Option<String>, bool) {
        let lines: Vec<String> =
            std::fs::read_to_string(log).unwrap_or_default().lines().map(str::to_string).collect();
        let name = lines.iter().find(|line| line.starts_with("run ")).and_then(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.windows(2).find(|pair| pair[0] == "--name").map(|pair| pair[1].to_string())
        });
        let killed = name
            .as_ref()
            .is_some_and(|name| lines.iter().any(|line| *line == format!("kill {name}")));
        (name, killed)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_container_is_killed_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, log) = logging_runtime(dir.path());
        let executor = ContainerCommandExecutor::new(ContainerConfig {
            runtime: runtime.display().to_string(),
            ..ContainerConfig::default()
        });

        let result = executor
            .execute(python_request("while True: pass", std::time::Duration::from_millis(1500)))
            .await
            .unwrap();

        assert_eq!(result.status, ExecutionStatus::Timeout);
        let (name, killed) = killed_container(&log);
        assert!(
            name.is_some() && killed,
            "{name:?} not killed: {:?}",
            std::fs::read_to_string(&log)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_execution_kills_its_container() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, log) = logging_runtime(dir.path());
        let executor = ContainerCommandExecutor::new(ContainerConfig {
            runtime: runtime.display().to_string(),
            ..ContainerConfig::default()
        });

        let execution = tokio::spawn(async move {
            executor
                .execute(python_request("while True: pass", std::time::Duration::from_secs(30)))
                .await
        });
        let mut started = false;
        for _ in 0..250 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            started = killed_container(&log).0.is_some();
            if started {
                break;
            }
        }
        assert!(started, "the container never started: {:?}", std::fs::read_to_string(&log));
        execution.abort();

        let mut killed = false;
        for _ in 0..250 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            killed = killed_container(&log).1;
            if killed {
                break;
            }
        }
        assert!(killed, "{:?}", std::fs::read_to_string(&log));
    }

    #[test]
    fn docker_config_presets() {
        let py = DockerConfig::python();
        assert_eq!(py.image, "python:3.12-slim");
        assert!(py.network_disabled);

        let node = DockerConfig::node();
        assert_eq!(node.image, "node:20-slim");

        let custom = DockerConfig::custom("ubuntu:24.04");
        assert_eq!(custom.image, "ubuntu:24.04");
    }

    fn policy(
        network: NetworkPolicy,
        filesystem: FilesystemPolicy,
        environment: EnvironmentPolicy,
    ) -> SandboxPolicy {
        SandboxPolicy { network, filesystem, environment, ..SandboxPolicy::strict_rust() }
    }

    fn assert_rejected(config: &DockerConfig, policy: &SandboxPolicy, mentions: &str) {
        match config.validate_sandbox_policy(policy) {
            Err(ExecutionError::UnsupportedPolicy(message)) => {
                assert!(message.contains(mentions), "{message}");
            }
            other => panic!("expected UnsupportedPolicy mentioning `{mentions}`, got {other:?}"),
        }
    }

    #[test]
    fn docker_policy_default_config_satisfies_strict_policy() {
        assert_eq!(
            DockerConfig::python().validate_sandbox_policy(&SandboxPolicy::strict_rust()),
            Ok(())
        );
    }

    #[test]
    fn docker_policy_rejects_disabled_network_on_networked_container() {
        let config = DockerConfig::python().with_network();
        assert_rejected(&config, &SandboxPolicy::strict_rust(), "network");

        let allows_network =
            policy(NetworkPolicy::Enabled, FilesystemPolicy::None, EnvironmentPolicy::None);
        assert_eq!(config.validate_sandbox_policy(&allows_network), Ok(()));
    }

    #[test]
    fn docker_policy_allows_enabled_network_on_isolated_container() {
        let allows_network =
            policy(NetworkPolicy::Enabled, FilesystemPolicy::None, EnvironmentPolicy::None);
        assert_eq!(DockerConfig::python().validate_sandbox_policy(&allows_network), Ok(()));
    }

    #[test]
    fn docker_policy_rejects_any_mount_without_filesystem_access() {
        let config = DockerConfig::python().bind_mount("/srv/data:/data:ro");
        assert_rejected(&config, &SandboxPolicy::strict_rust(), "/srv/data:/data:ro");
    }

    #[test]
    fn docker_policy_read_only_workspace_requires_read_only_mounts_under_root() {
        let read_only = policy(
            NetworkPolicy::Disabled,
            FilesystemPolicy::WorkspaceReadOnly { root: PathBuf::from("/srv") },
            EnvironmentPolicy::None,
        );

        let ro_under_root = DockerConfig::python().bind_mount("/srv/data:/data:ro");
        assert_eq!(ro_under_root.validate_sandbox_policy(&read_only), Ok(()));

        let rw_under_root = DockerConfig::python().bind_mount("/srv/data:/data");
        assert_rejected(&rw_under_root, &read_only, "/srv/data:/data");

        let ro_outside_root = DockerConfig::python().bind_mount("/home/user/.aws:/aws:ro");
        assert_rejected(&ro_outside_root, &read_only, "/home/user/.aws");

        let escapes_root = DockerConfig::python().bind_mount("/srv/../etc:/etc-host:ro");
        assert_rejected(&escapes_root, &read_only, "/srv/../etc");

        let sibling_prefix = DockerConfig::python().bind_mount("/srv-secrets:/s:ro");
        assert_rejected(&sibling_prefix, &read_only, "/srv-secrets");

        let named_volume = DockerConfig::python().bind_mount("cache:/cache:ro");
        assert_rejected(&named_volume, &read_only, "cache:/cache:ro");
    }

    #[test]
    fn docker_policy_read_write_workspace_accepts_any_mode_under_root() {
        let read_write = policy(
            NetworkPolicy::Disabled,
            FilesystemPolicy::WorkspaceReadWrite { root: PathBuf::from("/srv") },
            EnvironmentPolicy::None,
        );
        let config = DockerConfig::python()
            .bind_mount("/srv/data:/data")
            .bind_mount("/srv/models:/models:ro");
        assert_eq!(config.validate_sandbox_policy(&read_write), Ok(()));
    }

    #[test]
    fn docker_policy_explicit_paths_match_mode() {
        let paths = policy(
            NetworkPolicy::Disabled,
            FilesystemPolicy::Paths {
                read_only: vec![PathBuf::from("/srv/models")],
                read_write: vec![PathBuf::from("/srv/out")],
            },
            EnvironmentPolicy::None,
        );

        let config = DockerConfig::python()
            .bind_mount("/srv/models:/models:ro,z")
            .bind_mount("/srv/out:/out:rw");
        assert_eq!(config.validate_sandbox_policy(&paths), Ok(()));

        let writable_read_only_path = DockerConfig::python().bind_mount("/srv/models:/models");
        assert_rejected(&writable_read_only_path, &paths, "/srv/models:/models");
    }

    #[test]
    fn docker_policy_container_environment_must_be_allowlisted() {
        let config = DockerConfig::python().env("API_TOKEN=secret").env("MODE");
        assert_rejected(&config, &SandboxPolicy::strict_rust(), "API_TOKEN");

        let only_mode = policy(
            NetworkPolicy::Disabled,
            FilesystemPolicy::None,
            EnvironmentPolicy::AllowList(vec!["MODE".to_string()]),
        );
        assert_rejected(&config, &only_mode, "API_TOKEN");

        let both = policy(
            NetworkPolicy::Disabled,
            FilesystemPolicy::None,
            EnvironmentPolicy::AllowList(vec!["API_TOKEN".to_string(), "MODE".to_string()]),
        );
        assert_eq!(config.validate_sandbox_policy(&both), Ok(()));
    }

    #[test]
    fn docker_config_builder_methods() {
        let config = DockerConfig::python()
            .pip_install(&["numpy", "pandas"])
            .with_network()
            .env("MY_VAR=hello")
            .bind_mount("/host/data:/data:ro");

        assert!(!config.network_disabled);
        assert_eq!(config.setup_commands.len(), 1);
        assert!(config.setup_commands[0].contains("numpy"));
        assert_eq!(config.environment, vec!["MY_VAR=hello"]);
        assert_eq!(config.bind_mounts, vec!["/host/data:/data:ro"]);
    }
}
