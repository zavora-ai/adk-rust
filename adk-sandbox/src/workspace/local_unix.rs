//! Local Unix sandbox client implementation.
//!
//! Provisions workspaces as temporary directories on the local filesystem,
//! executes commands via child processes, and snapshots via tar archives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::client::SandboxClient;
use super::manifest::{Manifest, ManifestEntry};
use super::path_safety::validate_relative_path;
use super::session::SandboxSession;
use super::types::{DirEntry, EntryType, ExecOutput, SessionHandle, SnapshotId};
use crate::SandboxError;

/// SandboxClient implementation using local filesystem directories.
///
/// Provisions workspaces as temporary directories, executes commands
/// via child processes, and snapshots via tar archives.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::{LocalUnixClient, Manifest, SandboxClient};
/// use std::path::PathBuf;
///
/// let client = LocalUnixClient {
///     base_dir: None,
///     snapshot_dir: PathBuf::from("/tmp/snapshots"),
///     sessions: Default::default(),
/// };
///
/// let handle = client.provision(&Manifest { entries: vec![] }).await?;
/// ```
pub struct LocalUnixClient {
    /// Base directory for workspace temp dirs (defaults to system temp).
    pub base_dir: Option<PathBuf>,
    /// Base directory for snapshot archives.
    pub snapshot_dir: PathBuf,
    /// Active sessions mapping handle IDs to workspace paths.
    pub sessions: RwLock<HashMap<String, PathBuf>>,
}

impl LocalUnixClient {
    /// Creates a new `LocalUnixClient` with the given snapshot directory.
    ///
    /// If `base_dir` is `None`, temporary directories are created in the
    /// system's default temp location.
    pub fn new(base_dir: Option<PathBuf>, snapshot_dir: PathBuf) -> Self {
        Self { base_dir, snapshot_dir, sessions: RwLock::new(HashMap::new()) }
    }

    /// Generates a unique session handle ID using UUID v4.
    fn generate_session_id() -> String {
        format!("session-{}", uuid::Uuid::new_v4())
    }
}

/// Default command timeout (120 seconds).
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// A live sandbox session backed by a local Unix filesystem directory.
///
/// Provides workspace operations (exec, read, write, list, patch) against
/// a local directory. Commands are executed via child processes with
/// configurable timeouts.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::{LocalUnixClient, Manifest, SandboxClient};
///
/// let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
/// let handle = client.provision(&Manifest { entries: vec![] }).await?;
/// let session = client.start(&handle).await?;
///
/// let output = session.exec_command("echo hello", None).await?;
/// assert_eq!(output.stdout.trim(), "hello");
/// ```
pub struct LocalUnixSession {
    /// The root directory of the workspace.
    pub workspace_dir: PathBuf,
    /// Maximum duration for individual command executions.
    pub command_timeout: Duration,
}

impl LocalUnixSession {
    /// Creates a new `LocalUnixSession` with the given workspace directory
    /// and command timeout.
    pub fn new(workspace_dir: PathBuf, command_timeout: Duration) -> Self {
        Self { workspace_dir, command_timeout }
    }
}

impl std::fmt::Debug for LocalUnixSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalUnixSession")
            .field("workspace_dir", &self.workspace_dir)
            .field("command_timeout", &self.command_timeout)
            .finish()
    }
}

#[async_trait]
impl SandboxSession for LocalUnixSession {
    async fn exec_command(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        let cwd = match working_dir {
            Some(dir) => {
                validate_relative_path(dir)?;
                self.workspace_dir.join(dir)
            }
            None => self.workspace_dir.clone(),
        };

        let start = std::time::Instant::now();

        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| SandboxError::ExecutionFailed(format!("failed to spawn command: {e}")))?;

        // Take stdout/stderr handles before waiting so we can still kill on timeout
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        match tokio::time::timeout(self.command_timeout, child.wait()).await {
            Ok(Ok(status)) => {
                let duration = start.elapsed();
                let stdout_bytes = if let Some(mut out) = stdout {
                    use tokio::io::AsyncReadExt;
                    let mut buf = Vec::new();
                    let _ = out.read_to_end(&mut buf).await;
                    buf
                } else {
                    Vec::new()
                };
                let stderr_bytes = if let Some(mut err) = stderr {
                    use tokio::io::AsyncReadExt;
                    let mut buf = Vec::new();
                    let _ = err.read_to_end(&mut buf).await;
                    buf
                } else {
                    Vec::new()
                };
                Ok(ExecOutput::new(
                    String::from_utf8_lossy(&stdout_bytes).into_owned(),
                    String::from_utf8_lossy(&stderr_bytes).into_owned(),
                    status.code().unwrap_or(-1),
                    duration,
                    false,
                ))
            }
            Ok(Err(e)) => {
                Err(SandboxError::ExecutionFailed(format!("command execution failed: {e}")))
            }
            Err(_) => {
                // Timeout: kill the process
                let _ = child.kill().await;
                let duration = start.elapsed();
                Ok(ExecOutput::new("", "", -1, duration, true))
            }
        }
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, SandboxError> {
        validate_relative_path(path)?;
        let full_path = self.workspace_dir.join(path);
        tokio::fs::read(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SandboxError::ExecutionFailed(format!("file not found: {path}"))
            } else {
                SandboxError::ExecutionFailed(format!("failed to read file '{path}': {e}"))
            }
        })
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxError> {
        validate_relative_path(path)?;
        let full_path = self.workspace_dir.join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                SandboxError::ExecutionFailed(format!(
                    "failed to create parent directories for '{path}': {e}"
                ))
            })?;
        }
        tokio::fs::write(&full_path, content).await.map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to write file '{path}': {e}"))
        })
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, SandboxError> {
        validate_relative_path(path)?;
        let full_path = self.workspace_dir.join(path);
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SandboxError::ExecutionFailed(format!("directory not found: {path}"))
            } else {
                SandboxError::ExecutionFailed(format!("failed to read directory '{path}': {e}"))
            }
        })?;

        while let Some(entry) = read_dir.next_entry().await.map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to read directory entry: {e}"))
        })? {
            let file_type = entry.file_type().await.map_err(|e| {
                SandboxError::ExecutionFailed(format!("failed to get file type: {e}"))
            })?;
            let entry_type =
                if file_type.is_dir() { EntryType::Directory } else { EntryType::File };
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push(DirEntry::new(name, entry_type));
        }

        Ok(entries)
    }

    async fn apply_patch(&self, patch: &str) -> Result<(), SandboxError> {
        // Write patch to a temp file in the workspace
        let patch_file = self.workspace_dir.join(".adk_patch_tmp");
        tokio::fs::write(&patch_file, patch).await.map_err(|e| {
            SandboxError::ExecutionFailed(format!("failed to write patch file: {e}"))
        })?;

        let output = tokio::process::Command::new("patch")
            .arg("-p0")
            .arg("--no-backup-if-mismatch")
            .arg("-i")
            .arg(&patch_file)
            .current_dir(&self.workspace_dir)
            .output()
            .await
            .map_err(|e| {
                SandboxError::ExecutionFailed(format!("failed to execute patch command: {e}"))
            })?;

        // Clean up the temp patch file
        let _ = tokio::fs::remove_file(&patch_file).await;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(SandboxError::ExecutionFailed(format!("patch failed: {stderr}")))
        }
    }
}

#[async_trait]
impl SandboxClient for LocalUnixClient {
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError> {
        // Create temp directory (in base_dir if specified, else system temp)
        let workspace_dir = match &self.base_dir {
            Some(base) => {
                tokio::fs::create_dir_all(base).await.map_err(|e| {
                    SandboxError::ProvisionFailed {
                        resource: base.display().to_string(),
                        reason: format!("failed to create base directory: {e}"),
                        suggestion: "Ensure the base directory path is writable.".to_string(),
                    }
                })?;
                tempfile::tempdir_in(base)
            }
            None => tempfile::tempdir(),
        }
        .map_err(|e| SandboxError::ProvisionFailed {
            resource: "workspace".to_string(),
            reason: format!("failed to create temp directory: {e}"),
            suggestion: "Ensure the temp directory is writable.".to_string(),
        })?;

        let workspace_path = workspace_dir.keep();

        // Process each manifest entry
        for entry in &manifest.entries {
            match entry {
                ManifestEntry::File { path, content } => {
                    validate_relative_path(path)?;
                    let target = workspace_path.join(path);
                    if let Some(parent) = target.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            SandboxError::ProvisionFailed {
                                resource: path.clone(),
                                reason: format!("failed to create parent dirs: {e}"),
                                suggestion: "Check filesystem permissions.".to_string(),
                            }
                        })?;
                    }
                    tokio::fs::write(&target, content).await.map_err(|e| {
                        SandboxError::ProvisionFailed {
                            resource: path.clone(),
                            reason: format!("failed to write file: {e}"),
                            suggestion: "Check filesystem permissions.".to_string(),
                        }
                    })?;
                }

                ManifestEntry::Directory { path } => {
                    validate_relative_path(path)?;
                    let target = workspace_path.join(path);
                    tokio::fs::create_dir_all(&target).await.map_err(|e| {
                        SandboxError::ProvisionFailed {
                            resource: path.clone(),
                            reason: format!("failed to create directory: {e}"),
                            suggestion: "Check filesystem permissions.".to_string(),
                        }
                    })?;
                }

                ManifestEntry::GitRepo { url, branch, path } => {
                    validate_relative_path(path)?;
                    let target = workspace_path.join(path);

                    // Clone the repository
                    let mut cmd = tokio::process::Command::new("git");
                    cmd.arg("clone").arg(url).arg(&target);
                    let output = cmd.output().await.map_err(|e| SandboxError::ProvisionFailed {
                        resource: path.clone(),
                        reason: format!("git clone failed to execute: {e}"),
                        suggestion: "Ensure git is installed and accessible.".to_string(),
                    })?;

                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(SandboxError::ProvisionFailed {
                            resource: path.clone(),
                            reason: format!("git clone failed: {stderr}"),
                            suggestion: "Check the repository URL and network connectivity."
                                .to_string(),
                        });
                    }

                    // Optionally checkout a specific branch
                    if let Some(branch_name) = branch {
                        let mut checkout_cmd = tokio::process::Command::new("git");
                        checkout_cmd.arg("checkout").arg(branch_name).current_dir(&target);
                        let checkout_output = checkout_cmd.output().await.map_err(|e| {
                            SandboxError::ProvisionFailed {
                                resource: path.clone(),
                                reason: format!("git checkout failed to execute: {e}"),
                                suggestion: "Ensure git is installed.".to_string(),
                            }
                        })?;

                        if !checkout_output.status.success() {
                            let stderr = String::from_utf8_lossy(&checkout_output.stderr);
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
        sessions.insert(session_id, workspace_path);

        Ok(handle)
    }

    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError> {
        let sessions = self.sessions.read().await;
        let workspace_path = sessions
            .get(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?;
        Ok(Box::new(LocalUnixSession::new(workspace_path.clone(), DEFAULT_COMMAND_TIMEOUT)))
    }

    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        let mut sessions = self.sessions.write().await;
        let workspace_path = sessions
            .remove(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?;

        // Clean up the workspace directory since the session is done
        if workspace_path.exists() {
            tokio::fs::remove_dir_all(&workspace_path).await.map_err(|e| {
                SandboxError::ProvisionFailed {
                    resource: workspace_path.display().to_string(),
                    reason: format!("failed to remove workspace directory: {e}"),
                    suggestion: "Check filesystem permissions.".to_string(),
                }
            })?;
        }

        Ok(())
    }

    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError> {
        let sessions = self.sessions.read().await;
        let workspace_path = sessions
            .get(handle.as_str())
            .ok_or_else(|| SandboxError::SessionNotFound { handle: handle.as_str().to_string() })?
            .clone();
        drop(sessions);

        // Ensure snapshot_dir exists
        tokio::fs::create_dir_all(&self.snapshot_dir).await.map_err(|e| {
            SandboxError::ProvisionFailed {
                resource: self.snapshot_dir.display().to_string(),
                reason: format!("failed to create snapshot directory: {e}"),
                suggestion: "Ensure the snapshot directory path is writable.".to_string(),
            }
        })?;

        // Generate a unique snapshot ID
        let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4());
        let archive_path = self.snapshot_dir.join(format!("{snapshot_id}.tar"));

        // Create tar archive using spawn_blocking to avoid blocking the async runtime
        let workspace = workspace_path.clone();
        let archive = archive_path.clone();
        tokio::task::spawn_blocking(move || {
            let file =
                std::fs::File::create(&archive).map_err(|e| SandboxError::ProvisionFailed {
                    resource: archive.display().to_string(),
                    reason: format!("failed to create snapshot archive: {e}"),
                    suggestion: "Ensure the snapshot directory is writable.".to_string(),
                })?;
            let mut builder = tar::Builder::new(file);
            builder.append_dir_all(".", &workspace).map_err(|e| SandboxError::ProvisionFailed {
                resource: workspace.display().to_string(),
                reason: format!("failed to archive workspace: {e}"),
                suggestion: "Ensure workspace files are readable.".to_string(),
            })?;
            builder.finish().map_err(|e| SandboxError::ProvisionFailed {
                resource: archive.display().to_string(),
                reason: format!("failed to finalize snapshot archive: {e}"),
                suggestion: "Check disk space and permissions.".to_string(),
            })?;
            Ok::<(), SandboxError>(())
        })
        .await
        .map_err(|e| SandboxError::ProvisionFailed {
            resource: "snapshot".to_string(),
            reason: format!("snapshot task panicked: {e}"),
            suggestion: "This is an internal error.".to_string(),
        })??;

        Ok(SnapshotId::new(snapshot_id))
    }

    async fn resume(&self, snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError> {
        let archive_path = self.snapshot_dir.join(format!("{}.tar", snapshot_id.as_str()));

        // Verify the snapshot archive exists
        if !archive_path.exists() {
            return Err(SandboxError::SnapshotNotFound { id: snapshot_id.as_str().to_string() });
        }

        // Create a new temp directory (same logic as provision)
        let workspace_dir = match &self.base_dir {
            Some(base) => {
                tokio::fs::create_dir_all(base).await.map_err(|e| {
                    SandboxError::ProvisionFailed {
                        resource: base.display().to_string(),
                        reason: format!("failed to create base directory: {e}"),
                        suggestion: "Ensure the base directory path is writable.".to_string(),
                    }
                })?;
                tempfile::tempdir_in(base)
            }
            None => tempfile::tempdir(),
        }
        .map_err(|e| SandboxError::ProvisionFailed {
            resource: "workspace".to_string(),
            reason: format!("failed to create temp directory for resume: {e}"),
            suggestion: "Ensure the temp directory is writable.".to_string(),
        })?;

        let workspace_path = workspace_dir.keep();

        // Extract the tar archive into the new workspace directory
        let archive = archive_path.clone();
        let workspace = workspace_path.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&archive).map_err(|e| {
                SandboxError::SnapshotNotFound { id: format!("failed to open archive: {e}") }
            })?;
            let mut archive = tar::Archive::new(file);
            archive.unpack(&workspace).map_err(|e| SandboxError::ProvisionFailed {
                resource: workspace.display().to_string(),
                reason: format!("failed to extract snapshot archive: {e}"),
                suggestion: "Ensure the workspace directory is writable.".to_string(),
            })?;
            Ok::<(), SandboxError>(())
        })
        .await
        .map_err(|e| SandboxError::ProvisionFailed {
            resource: "resume".to_string(),
            reason: format!("resume task panicked: {e}"),
            suggestion: "This is an internal error.".to_string(),
        })??;

        // Generate a new session handle and store in sessions map
        let session_id = Self::generate_session_id();
        let handle = SessionHandle::new(&session_id);

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id, workspace_path);

        Ok(handle)
    }
}

impl std::fmt::Debug for LocalUnixClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalUnixClient")
            .field("base_dir", &self.base_dir)
            .field("snapshot_dir", &self.snapshot_dir)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provision_empty_manifest() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest { entries: vec![] };
        let handle = client.provision(&manifest).await.unwrap();
        assert!(handle.as_str().starts_with("session-"));

        // Verify session is tracked
        let sessions = client.sessions.read().await;
        assert!(sessions.contains_key(handle.as_str()));
    }

    #[tokio::test]
    async fn provision_with_file_entry() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest {
            entries: vec![ManifestEntry::File {
                path: "src/main.rs".to_string(),
                content: b"fn main() {}".to_vec(),
            }],
        };
        let handle = client.provision(&manifest).await.unwrap();

        let sessions = client.sessions.read().await;
        let workspace_path = sessions.get(handle.as_str()).unwrap();
        let content = tokio::fs::read(workspace_path.join("src/main.rs")).await.unwrap();
        assert_eq!(content, b"fn main() {}");
    }

    #[tokio::test]
    async fn provision_with_directory_entry() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest =
            Manifest { entries: vec![ManifestEntry::Directory { path: "src/utils".to_string() }] };
        let handle = client.provision(&manifest).await.unwrap();

        let sessions = client.sessions.read().await;
        let workspace_path = sessions.get(handle.as_str()).unwrap();
        assert!(workspace_path.join("src/utils").is_dir());
    }

    #[tokio::test]
    async fn provision_rejects_path_traversal() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest {
            entries: vec![ManifestEntry::File {
                path: "../escape.txt".to_string(),
                content: b"bad".to_vec(),
            }],
        };
        let result = client.provision(&manifest).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "../escape.txt");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn provision_rejects_absolute_path() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest =
            Manifest { entries: vec![ManifestEntry::Directory { path: "/etc/bad".to_string() }] };
        let result = client.provision(&manifest).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "/etc/bad");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_removes_session() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest { entries: vec![] };
        let handle = client.provision(&manifest).await.unwrap();

        // Verify workspace directory exists before stop
        let workspace_path = {
            let sessions = client.sessions.read().await;
            sessions.get(handle.as_str()).unwrap().clone()
        };
        assert!(workspace_path.exists());

        client.stop(&handle).await.unwrap();

        // Verify session is removed from map
        let sessions = client.sessions.read().await;
        assert!(!sessions.contains_key(handle.as_str()));

        // Verify workspace directory is cleaned up
        assert!(!workspace_path.exists());
    }

    #[tokio::test]
    async fn stop_unknown_session_returns_error() {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let handle = SessionHandle::new("nonexistent-session");
        let result = client.stop(&handle).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::SessionNotFound { handle: h } => {
                assert_eq!(h, "nonexistent-session");
            }
            other => panic!("expected SessionNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn provision_with_base_dir() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().to_path_buf();
        let client = LocalUnixClient::new(Some(base.clone()), PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest {
            entries: vec![ManifestEntry::File {
                path: "hello.txt".to_string(),
                content: b"world".to_vec(),
            }],
        };
        let handle = client.provision(&manifest).await.unwrap();

        let sessions = client.sessions.read().await;
        let workspace_path = sessions.get(handle.as_str()).unwrap();
        // Workspace should be inside the base dir
        assert!(workspace_path.starts_with(&base));
        let content = tokio::fs::read(workspace_path.join("hello.txt")).await.unwrap();
        assert_eq!(content, b"world");
    }

    #[tokio::test]
    async fn snapshot_creates_tar_archive() {
        let snapshot_dir = tempfile::tempdir().unwrap();
        let client = LocalUnixClient::new(None, snapshot_dir.path().to_path_buf());
        let manifest = Manifest {
            entries: vec![ManifestEntry::File {
                path: "data.txt".to_string(),
                content: b"snapshot me".to_vec(),
            }],
        };
        let handle = client.provision(&manifest).await.unwrap();

        let snap_id = client.snapshot(&handle).await.unwrap();
        assert!(snap_id.as_str().starts_with("snap-"));

        // Verify the tar archive was created
        let archive_path = snapshot_dir.path().join(format!("{}.tar", snap_id.as_str()));
        assert!(archive_path.exists());
    }

    #[tokio::test]
    async fn snapshot_unknown_session_returns_error() {
        let snapshot_dir = tempfile::tempdir().unwrap();
        let client = LocalUnixClient::new(None, snapshot_dir.path().to_path_buf());
        let handle = SessionHandle::new("nonexistent-session");
        let result = client.snapshot(&handle).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::SessionNotFound { handle: h } => {
                assert_eq!(h, "nonexistent-session");
            }
            other => panic!("expected SessionNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_restores_workspace_from_snapshot() {
        let snapshot_dir = tempfile::tempdir().unwrap();
        let client = LocalUnixClient::new(None, snapshot_dir.path().to_path_buf());
        let manifest = Manifest {
            entries: vec![
                ManifestEntry::File {
                    path: "src/main.rs".to_string(),
                    content: b"fn main() { println!(\"hello\"); }".to_vec(),
                },
                ManifestEntry::File {
                    path: "README.md".to_string(),
                    content: b"# My Project".to_vec(),
                },
                ManifestEntry::Directory { path: "tests".to_string() },
            ],
        };
        let handle = client.provision(&manifest).await.unwrap();

        // Snapshot the session
        let snap_id = client.snapshot(&handle).await.unwrap();

        // Resume from the snapshot
        let resumed_handle = client.resume(&snap_id).await.unwrap();

        // Verify the resumed session has a different handle
        assert_ne!(handle.as_str(), resumed_handle.as_str());
        assert!(resumed_handle.as_str().starts_with("session-"));

        // Verify file contents are restored
        let sessions = client.sessions.read().await;
        let resumed_workspace = sessions.get(resumed_handle.as_str()).unwrap();

        let main_content = tokio::fs::read(resumed_workspace.join("src/main.rs")).await.unwrap();
        assert_eq!(main_content, b"fn main() { println!(\"hello\"); }");

        let readme_content = tokio::fs::read(resumed_workspace.join("README.md")).await.unwrap();
        assert_eq!(readme_content, b"# My Project");

        // Verify directory was restored
        assert!(resumed_workspace.join("tests").is_dir());
    }

    #[tokio::test]
    async fn resume_nonexistent_snapshot_returns_error() {
        let snapshot_dir = tempfile::tempdir().unwrap();
        let client = LocalUnixClient::new(None, snapshot_dir.path().to_path_buf());
        let snap_id = SnapshotId::new("snap-nonexistent");
        let result = client.resume(&snap_id).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::SnapshotNotFound { id } => {
                assert_eq!(id, "snap-nonexistent");
            }
            other => panic!("expected SnapshotNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_resume_roundtrip_preserves_modified_files() {
        let snapshot_dir = tempfile::tempdir().unwrap();
        let client = LocalUnixClient::new(None, snapshot_dir.path().to_path_buf());
        let manifest = Manifest {
            entries: vec![ManifestEntry::File {
                path: "config.toml".to_string(),
                content: b"version = 1".to_vec(),
            }],
        };
        let handle = client.provision(&manifest).await.unwrap();

        // Modify a file in the workspace after provisioning
        let workspace_path = {
            let sessions = client.sessions.read().await;
            sessions.get(handle.as_str()).unwrap().clone()
        };
        tokio::fs::write(workspace_path.join("config.toml"), b"version = 2").await.unwrap();
        tokio::fs::write(workspace_path.join("new_file.txt"), b"added after provision")
            .await
            .unwrap();

        // Snapshot captures the modified state
        let snap_id = client.snapshot(&handle).await.unwrap();

        // Resume and verify modified state is restored
        let resumed_handle = client.resume(&snap_id).await.unwrap();
        let sessions = client.sessions.read().await;
        let resumed_workspace = sessions.get(resumed_handle.as_str()).unwrap();

        let config_content = tokio::fs::read(resumed_workspace.join("config.toml")).await.unwrap();
        assert_eq!(config_content, b"version = 2");

        let new_file_content =
            tokio::fs::read(resumed_workspace.join("new_file.txt")).await.unwrap();
        assert_eq!(new_file_content, b"added after provision");
    }

    // ── LocalUnixSession tests ──────────────────────────────────────────────

    /// Helper to create a session with a provisioned workspace.
    async fn create_test_session() -> (LocalUnixClient, SessionHandle, Box<dyn SandboxSession>) {
        let client = LocalUnixClient::new(None, PathBuf::from("/tmp/snapshots"));
        let manifest = Manifest {
            entries: vec![
                ManifestEntry::File {
                    path: "hello.txt".to_string(),
                    content: b"Hello, world!".to_vec(),
                },
                ManifestEntry::File {
                    path: "src/main.rs".to_string(),
                    content: b"fn main() {}".to_vec(),
                },
                ManifestEntry::Directory { path: "empty_dir".to_string() },
            ],
        };
        let handle = client.provision(&manifest).await.unwrap();
        let session = client.start(&handle).await.unwrap();
        (client, handle, session)
    }

    #[tokio::test]
    async fn session_start_returns_session() {
        let (_client, _handle, session) = create_test_session().await;
        // Session should be usable — verify by reading a file
        let content = session.read_file("hello.txt").await.unwrap();
        assert_eq!(content, b"Hello, world!");
    }

    #[tokio::test]
    async fn session_exec_command_basic() {
        let (_client, _handle, session) = create_test_session().await;
        let output = session.exec_command("echo hello", None).await.unwrap();
        assert_eq!(output.stdout.trim(), "hello");
        assert_eq!(output.exit_code, 0);
        assert!(!output.timed_out);
    }

    #[tokio::test]
    async fn session_exec_command_with_working_dir() {
        let (_client, _handle, session) = create_test_session().await;
        let output = session.exec_command("ls main.rs", Some("src")).await.unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("main.rs"));
    }

    #[tokio::test]
    async fn session_exec_command_captures_stderr() {
        let (_client, _handle, session) = create_test_session().await;
        let output = session.exec_command("echo error >&2", None).await.unwrap();
        assert_eq!(output.stderr.trim(), "error");
        assert_eq!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn session_exec_command_nonzero_exit() {
        let (_client, _handle, session) = create_test_session().await;
        let output = session.exec_command("exit 42", None).await.unwrap();
        assert_eq!(output.exit_code, 42);
        assert!(!output.timed_out);
    }

    #[tokio::test]
    async fn session_exec_command_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let session = LocalUnixSession::new(
            temp.path().to_path_buf(),
            Duration::from_millis(100), // Very short timeout
        );
        let output = session.exec_command("sleep 10", None).await.unwrap();
        assert!(output.timed_out);
        assert_eq!(output.exit_code, -1);
    }

    #[tokio::test]
    async fn session_exec_command_invalid_working_dir() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.exec_command("ls", Some("../escape")).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "../escape");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_read_file_success() {
        let (_client, _handle, session) = create_test_session().await;
        let content = session.read_file("hello.txt").await.unwrap();
        assert_eq!(content, b"Hello, world!");
    }

    #[tokio::test]
    async fn session_read_file_nested() {
        let (_client, _handle, session) = create_test_session().await;
        let content = session.read_file("src/main.rs").await.unwrap();
        assert_eq!(content, b"fn main() {}");
    }

    #[tokio::test]
    async fn session_read_file_not_found() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.read_file("nonexistent.txt").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::ExecutionFailed(msg) => {
                assert!(msg.contains("not found"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_read_file_path_traversal() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.read_file("../etc/passwd").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "../etc/passwd");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_write_file_new() {
        let (_client, _handle, session) = create_test_session().await;
        session.write_file("new_file.txt", b"new content").await.unwrap();
        let content = session.read_file("new_file.txt").await.unwrap();
        assert_eq!(content, b"new content");
    }

    #[tokio::test]
    async fn session_write_file_creates_parent_dirs() {
        let (_client, _handle, session) = create_test_session().await;
        session.write_file("deep/nested/dir/file.txt", b"deep content").await.unwrap();
        let content = session.read_file("deep/nested/dir/file.txt").await.unwrap();
        assert_eq!(content, b"deep content");
    }

    #[tokio::test]
    async fn session_write_file_overwrites_existing() {
        let (_client, _handle, session) = create_test_session().await;
        session.write_file("hello.txt", b"overwritten").await.unwrap();
        let content = session.read_file("hello.txt").await.unwrap();
        assert_eq!(content, b"overwritten");
    }

    #[tokio::test]
    async fn session_write_file_path_traversal() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.write_file("../../escape.txt", b"bad").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "../../escape.txt");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_list_dir_root() {
        let (_client, _handle, session) = create_test_session().await;
        let entries = session.list_dir(".").await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello.txt"));
        assert!(names.contains(&"src"));
        assert!(names.contains(&"empty_dir"));
    }

    #[tokio::test]
    async fn session_list_dir_subdirectory() {
        let (_client, _handle, session) = create_test_session().await;
        let entries = session.list_dir("src").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "main.rs");
        assert_eq!(entries[0].entry_type, EntryType::File);
    }

    #[tokio::test]
    async fn session_list_dir_entry_types() {
        let (_client, _handle, session) = create_test_session().await;
        let entries = session.list_dir(".").await.unwrap();

        let src_entry = entries.iter().find(|e| e.name == "src").unwrap();
        assert_eq!(src_entry.entry_type, EntryType::Directory);

        let hello_entry = entries.iter().find(|e| e.name == "hello.txt").unwrap();
        assert_eq!(hello_entry.entry_type, EntryType::File);
    }

    #[tokio::test]
    async fn session_list_dir_not_found() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.list_dir("nonexistent").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::ExecutionFailed(msg) => {
                assert!(msg.contains("not found"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_list_dir_path_traversal() {
        let (_client, _handle, session) = create_test_session().await;
        let result = session.list_dir("../..").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => {
                assert_eq!(path, "../..");
            }
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_apply_patch_success() {
        let (_client, _handle, session) = create_test_session().await;
        // Write a file to patch
        session.write_file("target.txt", b"line1\nline2\nline3\n").await.unwrap();

        let patch = "\
--- target.txt
+++ target.txt
@@ -1,3 +1,3 @@
 line1
-line2
+modified_line2
 line3
";
        session.apply_patch(patch).await.unwrap();

        let content = session.read_file("target.txt").await.unwrap();
        let text = String::from_utf8(content).unwrap();
        assert!(text.contains("modified_line2"));
        assert!(!text.contains("\nline2\n"));
    }

    #[tokio::test]
    async fn session_apply_patch_failure() {
        let (_client, _handle, session) = create_test_session().await;
        // Apply a patch that doesn't match any file
        let bad_patch = "\
--- nonexistent.txt
+++ nonexistent.txt
@@ -1,3 +1,3 @@
 foo
-bar
+baz
 qux
";
        let result = session.apply_patch(bad_patch).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::ExecutionFailed(msg) => {
                assert!(msg.contains("patch failed"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_write_read_roundtrip() {
        let (_client, _handle, session) = create_test_session().await;
        let content = b"arbitrary binary content \x00\x01\x02\xff";
        session.write_file("binary.dat", content).await.unwrap();
        let read_back = session.read_file("binary.dat").await.unwrap();
        assert_eq!(read_back, content);
    }
}
