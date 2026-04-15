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
//! | Capability | Enforced | Mechanism |
//! |---|---|---|
//! | Network policy | Yes | `--network=none` on container create |
//! | Filesystem policy | Yes | Explicit bind mounts only |
//! | Environment policy | Yes | Only specified env vars |
//! | Timeout | Yes | `tokio::time::timeout` on exec |
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

use std::time::Instant;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::{
    BackendCapabilities, CodeExecutor, EnvironmentPolicy, ExecutionError, ExecutionIsolation,
    ExecutionLanguage, ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus,
    FilesystemPolicy, NetworkPolicy, validate_request,
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
            self.exec_in_container(container_id, &cmd, None).await?;
            Ok(())
        }

        /// Execute a command inside the running container and capture output.
        async fn exec_in_container(
            &self,
            container_id: &str,
            cmd: &[String],
            timeout: Option<std::time::Duration>,
        ) -> Result<(String, String, Option<i64>), ExecutionError> {
            let exec = self
                .docker
                .create_exec(
                    container_id,
                    CreateExecOptions {
                        cmd: Some(cmd.to_vec()),
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

            let exec_output = async {
                match self.docker.start_exec(&exec.id, None).await {
                    Ok(StartExecResults::Attached { mut output, .. }) => {
                        let mut stdout = String::new();
                        let mut stderr = String::new();

                        while let Some(chunk) = output.next().await {
                            match chunk {
                                Ok(bollard::container::LogOutput::StdOut { message }) => {
                                    stdout.push_str(&String::from_utf8_lossy(&message));
                                }
                                Ok(bollard::container::LogOutput::StdErr { message }) => {
                                    stderr.push_str(&String::from_utf8_lossy(&message));
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
                        let exit_code = inspect.exit_code;

                        Ok((stdout, stderr, exit_code))
                    }
                    Ok(StartExecResults::Detached) => Ok((String::new(), String::new(), None)),
                    Err(e) => {
                        Err(ExecutionError::ExecutionFailed(format!("failed to start exec: {e}")))
                    }
                }
            };

            if let Some(dur) = timeout {
                match tokio::time::timeout(dur, exec_output).await {
                    Ok(result) => result,
                    Err(_) => Err(ExecutionError::Timeout(dur.as_millis() as u64)),
                }
            } else {
                exec_output.await
            }
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

            // Build container config.
            let mut host_config = bollard::models::HostConfig::default();

            if self.config.network_disabled {
                host_config.network_mode = Some("none".to_string());
            }

            if !self.config.bind_mounts.is_empty() {
                host_config.binds = Some(self.config.bind_mounts.clone());
            }

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
            let _ = self.exec_in_container(&container_id, &mkdir_cmd, None).await;

            // Run setup commands.
            for setup_cmd in &self.config.setup_commands {
                info!(cmd = %setup_cmd, "running setup command");
                let cmd = vec!["sh".to_string(), "-c".to_string(), setup_cmd.clone()];
                let (_stdout, stderr, exit_code) =
                    self.exec_in_container(&container_id, &cmd, None).await?;

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

            // Execute with timeout.
            let (stdout, stderr, exit_code) = self
                .exec_in_container(&container_id, &exec_cmd, Some(request.sandbox.timeout))
                .await
                .map_err(|e| match e {
                    ExecutionError::Timeout(_) => e,
                    other => other,
                })?;

            let duration_ms = start.elapsed().as_millis() as u64;

            let (stdout, stdout_truncated) =
                truncate_output(stdout, request.sandbox.max_stdout_bytes);
            let (stderr, stderr_truncated) =
                truncate_output(stderr, request.sandbox.max_stderr_bytes);

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
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            runtime: "docker".to_string(),
            default_image: "python:3.12-slim".to_string(),
            extra_flags: vec![],
            auto_remove: true,
        }
    }
}

/// CLI-based container executor that shells out to `docker run` per execution.
///
/// Each [`execute`](CodeExecutor::execute) call spawns a new ephemeral container.
/// This is simpler but less efficient than `DockerExecutor` which reuses a
/// persistent container.
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
    fn build_run_args(&self, request: &ExecutionRequest) -> Vec<String> {
        let mut args = vec!["run".to_string()];

        if self.config.auto_remove {
            args.push("--rm".to_string());
        }

        args.push("-i".to_string());

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
        let run_args = self.build_run_args(&request);

        debug!(
            runtime = %self.config.runtime,
            image = %self.config.default_image,
            language = %request.language,
            "starting container execution"
        );

        let mut cmd = tokio::process::Command::new(&self.config.runtime);
        for arg in &run_args {
            cmd.arg(arg);
        }

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            ExecutionError::ExecutionFailed(format!(
                "failed to spawn container runtime '{}': {e}",
                self.config.runtime
            ))
        })?;

        if let Some(ref input) = request.input {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let json_bytes = serde_json::to_vec(input).unwrap_or_default();
                let _ = stdin.write_all(&json_bytes).await;
                drop(stdin);
            }
        } else if let Some(ref raw_stdin) = request.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(raw_stdin).await;
                drop(stdin);
            }
        } else {
            drop(child.stdin.take());
        }

        let output =
            match tokio::time::timeout(request.sandbox.timeout, child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    return Err(ExecutionError::ExecutionFailed(format!(
                        "failed to wait for container: {e}"
                    )));
                }
                Err(_) => {
                    warn!("container execution timed out");
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

        let args = executor.build_run_args(&request);
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

        let args = executor.build_run_args(&request);
        assert!(!args.contains(&"--network=none".to_string()));
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
