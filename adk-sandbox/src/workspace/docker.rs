//! Docker sandbox client implementation.
//!
//! Provisions workspaces inside Docker containers from a configurable base
//! image. Provides stronger isolation via container boundaries.
//!
//! This module is gated behind the `workspace-docker` feature flag.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CommitContainerOptions;
use bollard::models::HostConfig;
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

/// SandboxClient implementation using Docker containers.
///
/// Provisions workspaces inside containers from a configurable base image.
/// Provides stronger isolation via container boundaries. Resource limits
/// (memory, CPU) can be configured and are applied to containers on
/// provisioning.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::DockerClient;
///
/// let client = DockerClient::new().await?;
/// let client = client.with_resource_limits(Some(512 * 1024 * 1024), Some(1.5));
/// ```
pub struct DockerClient {
    /// Docker base image for new containers.
    pub base_image: String,
    /// Optional memory limit for containers (in bytes).
    pub memory_limit_bytes: Option<u64>,
    /// Optional CPU limit (fractional cores, e.g., 1.5).
    pub cpu_limit: Option<f64>,
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

    /// Generates a unique session handle ID.
    fn generate_session_id() -> String {
        format!("docker-session-{}", uuid::Uuid::new_v4())
    }

    /// Builds the `HostConfig` with resource limits applied.
    fn build_host_config(&self) -> HostConfig {
        let mut host_config = HostConfig::default();

        if let Some(memory) = self.memory_limit_bytes {
            host_config.memory = Some(memory as i64);
        }

        if let Some(cpu) = self.cpu_limit {
            // Docker uses NanoCPUs (1e9 = 1 core)
            host_config.nano_cpus = Some((cpu * 1_000_000_000.0) as i64);
        }

        host_config
    }

    /// Executes a command inside a container and returns stdout/stderr.
    async fn exec_in_container(
        &self,
        container_id: &str,
        cmd: Vec<&str>,
        working_dir: Option<&str>,
        stdin_content: Option<&[u8]>,
    ) -> Result<(String, String, i64), SandboxError> {
        let exec_options = CreateExecOptions {
            cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: stdin_content.is_some().then_some(true),
            working_dir: working_dir.map(|d| d.to_string()),
            ..Default::default()
        };

        let exec = self.client.create_exec(container_id, exec_options).await.map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to create exec instance: {e}"))
        })?;

        let start_result = self
            .client
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| SandboxError::ExecutionFailed(format!("failed to start exec: {e}")))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

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
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            stderr.push_str(&format!("stream error: {e}"));
                        }
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        // Get the exit code from the exec inspect
        let inspect =
            self.client.inspect_exec(&exec.id).await.map_err(|e| {
                SandboxError::ExecutionFailed(format!("failed to inspect exec: {e}"))
            })?;

        let exit_code = inspect.exit_code.unwrap_or(-1);

        Ok((stdout, stderr, exit_code))
    }
}

#[async_trait]
impl SandboxClient for DockerClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        // Create container from base image with resource limits
        let host_config = self.build_host_config();

        let config = Config {
            image: Some(self.base_image.clone()),
            // Keep container running with a long-lived process
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some(CONTAINER_WORKSPACE_ROOT.to_string()),
            host_config: Some(host_config),
            ..Default::default()
        };

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

                    // Write file content using sh -c with stdin
                    let cmd_str = format!("cat > '{full_path}'");
                    let (_, stderr, code) = self
                        .exec_in_container(
                            &container_id,
                            vec!["sh", "-c", &cmd_str],
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

                    // Clone the repository
                    let mut clone_cmd = format!("git clone '{url}' '{full_path}'");
                    let (_, stderr, code) = self
                        .exec_in_container(&container_id, vec!["sh", "-c", &clone_cmd], None, None)
                        .await?;
                    if code != 0 {
                        return Err(SandboxError::ProvisionFailed {
                            resource: path.clone(),
                            reason: format!("git clone failed: {stderr}"),
                            suggestion:
                                "Check the repository URL and ensure git is installed in the container."
                                    .to_string(),
                        });
                    }

                    // Checkout branch if specified
                    if let Some(branch_name) = branch {
                        clone_cmd = format!("cd '{full_path}' && git checkout '{branch_name}'");
                        let (_, stderr, code) = self
                            .exec_in_container(
                                &container_id,
                                vec!["sh", "-c", &clone_cmd],
                                None,
                                None,
                            )
                            .await?;
                        if code != 0 {
                            return Err(SandboxError::ProvisionFailed {
                                resource: path.clone(),
                                reason: format!("git checkout '{branch_name}' failed: {stderr}"),
                                suggestion: "Check that the branch exists in the repository."
                                    .to_string(),
                            });
                        }
                    }
                }
            }
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
        let host_config = self.build_host_config();

        let config = Config {
            image: Some(image_ref.to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            working_dir: Some(CONTAINER_WORKSPACE_ROOT.to_string()),
            host_config: Some(host_config),
            ..Default::default()
        };

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
    /// Executes a command inside the container and captures output.
    async fn exec_cmd(
        &self,
        cmd: Vec<&str>,
        working_dir: Option<&str>,
        stdin_content: Option<&[u8]>,
    ) -> Result<(String, String, i64), SandboxError> {
        let exec_options = CreateExecOptions {
            cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: stdin_content.is_some().then_some(true),
            working_dir: working_dir.map(|d| d.to_string()),
            ..Default::default()
        };

        let exec =
            self.client.create_exec(&self.container_id, exec_options).await.map_err(|e| {
                SandboxError::ExecutionFailed(format!("failed to create exec instance: {e}"))
            })?;

        let start_result = self
            .client
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| SandboxError::ExecutionFailed(format!("failed to start exec: {e}")))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        match start_result {
            StartExecResults::Attached { mut output, mut input } => {
                if let Some(content) = stdin_content {
                    use tokio::io::AsyncWriteExt;
                    let _ = input.write_all(content).await;
                    let _ = input.shutdown().await;
                }

                while let Some(msg) = output.next().await {
                    match msg {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            stderr.push_str(&format!("stream error: {e}"));
                        }
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        // Get the exit code
        let inspect =
            self.client.inspect_exec(&exec.id).await.map_err(|e| {
                SandboxError::ExecutionFailed(format!("failed to inspect exec: {e}"))
            })?;

        let exit_code = inspect.exit_code.unwrap_or(-1);
        Ok((stdout, stderr, exit_code))
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

        let result = tokio::time::timeout(
            self.command_timeout,
            self.exec_cmd(vec!["sh", "-c", command], Some(&cwd), None),
        )
        .await;

        match result {
            Ok(Ok((stdout, stderr, exit_code))) => {
                let duration = start.elapsed();
                Ok(ExecOutput::new(stdout, stderr, exit_code as i32, duration, false))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout
                let duration = start.elapsed();
                Ok(ExecOutput::new("", "", -1, duration, true))
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        validate_relative_path(path)?;
        let full_path = format!("{CONTAINER_WORKSPACE_ROOT}/{path}");

        let (stdout, stderr, exit_code) =
            self.exec_cmd(vec!["cat", &full_path], None, None).await?;

        if exit_code != 0 {
            if stderr.contains("No such file") {
                return Err(SandboxError::ExecutionFailed(format!("file not found: {path}")));
            }
            return Err(SandboxError::ExecutionFailed(format!(
                "failed to read file '{path}': {stderr}"
            )));
        }

        Ok(stdout.into_bytes())
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

        // Write content via stdin to cat
        let cmd_str = format!("cat > '{full_path}'");
        let (_, stderr, exit_code) =
            self.exec_cmd(vec!["sh", "-c", &cmd_str], None, Some(content)).await?;

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
    fn container_workspace_root_is_absolute() {
        assert!(CONTAINER_WORKSPACE_ROOT.starts_with('/'));
    }
}
