//! Sandbox client trait for workspace lifecycle management.
//!
//! The [`SandboxClient`] trait abstracts sandbox provisioning, session
//! management, and snapshot/resume capabilities. Implementations manage
//! the underlying sandbox infrastructure: local directories, Docker
//! containers, or remote environments (E2B with `remote-e2b`, Daytona with
//! `remote-daytona`, Modal with `remote-modal`).
//!
//! # Lifecycle
//!
//! ```text
//! provision(manifest) → SessionHandle           create
//!     start(handle) → Box<dyn SandboxSession>   attach (resumes a paused environment)
//!         ... exec, read_file, write_file, upload, download ...
//!         snapshot(handle) → SnapshotId
//!         pause(handle)                          where capabilities().pause
//!     stop(handle)                               destroy
//! resume(snapshot_id) → SessionHandle            create from a snapshot
//! ```

use async_trait::async_trait;

use super::session::SandboxSession;
use super::types::{ClientCapabilities, SessionHandle, SnapshotId};
use crate::error::SandboxError;
use crate::workspace::manifest::Manifest;

/// Async trait for sandbox lifecycle management.
///
/// Implementations provide provisioning, session management, and
/// snapshot/resume capabilities for workspace sandboxes.
///
/// # Requirements
///
/// - Implementations must be `Send + Sync` to support use across async
///   task boundaries.
/// - All methods return `Result<T, SandboxError>` for structured error
///   handling.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::{SandboxClient, Manifest, SessionHandle};
///
/// async fn run_agent(client: &dyn SandboxClient, manifest: &Manifest) {
///     let handle = client.provision(manifest).await.unwrap();
///     let session = client.start(&handle).await.unwrap();
///     // ... use session for exec_command, read_file, etc. ...
///     client.stop(&handle).await.unwrap();
/// }
/// ```
#[async_trait]
pub trait SandboxClient: Send + Sync {
    /// Provisions a new workspace from a manifest definition.
    ///
    /// Creates the workspace directory structure and populates it
    /// with all entries declared in the manifest (inline files,
    /// directories, and git repositories).
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::ProvisionFailed` if workspace creation
    /// or manifest entry population fails.
    /// Returns `SandboxError::PathTraversal` if any manifest entry
    /// specifies a path that escapes the workspace root.
    async fn provision(&self, manifest: &Manifest) -> Result<SessionHandle, SandboxError>;

    /// Starts a provisioned session, making it ready for operations.
    ///
    /// Returns a boxed [`SandboxSession`] trait object that provides
    /// workspace operations (exec, read, write, list, patch).
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::SessionNotFound` if the handle does not
    /// reference a valid provisioned session.
    ///
    /// Starting a handle another process provisioned attaches to that environment, for a
    /// client that can look it up by id (the remote providers can; `SessionHandle::new`
    /// with the provider's sandbox id builds the handle).
    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError>;

    /// Stops a running session, releasing associated resources.
    ///
    /// After stopping, the session handle is no longer valid for
    /// `start` operations. Any running child processes associated
    /// with the session are terminated. A remote provider's environment is destroyed, so
    /// it costs nothing further; snapshots taken from it remain.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::SessionNotFound` if the handle does not
    /// reference a valid session.
    async fn stop(&self, handle: &SessionHandle) -> Result<(), SandboxError>;

    /// Persists the current workspace state and returns an opaque snapshot ID.
    ///
    /// The snapshot captures the complete workspace filesystem state
    /// at the time of the call. The returned [`SnapshotId`] can be
    /// used with [`resume`](Self::resume) to restore the workspace later.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::SessionNotFound` if the handle does not
    /// reference a valid session.
    async fn snapshot(&self, handle: &SessionHandle) -> Result<SnapshotId, SandboxError>;

    /// Restores a workspace from a previously captured snapshot.
    ///
    /// Creates a new session with the workspace filesystem restored
    /// to the state captured at snapshot time. The returned
    /// [`SessionHandle`] can be used with [`start`](Self::start) to
    /// begin operations on the restored workspace.
    ///
    /// # Errors
    ///
    /// Returns `SandboxError::SnapshotNotFound` if the snapshot ID
    /// does not reference a valid snapshot.
    async fn resume(&self, snapshot_id: &SnapshotId) -> Result<SessionHandle, SandboxError>;

    /// The provider's name, such as `local`, `docker`, `e2b`, `daytona` or `modal`, as audit records
    /// and errors name it. The default is `custom`.
    fn provider(&self) -> &str {
        "custom"
    }

    /// What the client supports beyond provision, start, exec and stop. The default
    /// claims nothing.
    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities::default()
    }

    /// Suspends the environment so it stops costing compute; [`start`](Self::start)
    /// resumes it with its filesystem and, where the provider keeps it, its memory.
    ///
    /// # Errors
    ///
    /// The default returns `SandboxError::Unsupported`; a client whose
    /// [`capabilities`](Self::capabilities) report `pause` overrides it.
    async fn pause(&self, handle: &SessionHandle) -> Result<(), SandboxError> {
        let _ = handle;
        Err(SandboxError::Unsupported {
            provider: self.provider().to_string(),
            operation: "pause".to_string(),
        })
    }
}

impl std::fmt::Debug for dyn SandboxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<SandboxClient>")
    }
}
