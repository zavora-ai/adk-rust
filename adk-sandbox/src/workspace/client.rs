//! Sandbox client trait for workspace lifecycle management.
//!
//! The [`SandboxClient`] trait abstracts sandbox provisioning, session
//! management, and snapshot/resume capabilities. Implementations manage
//! the underlying sandbox infrastructure (local directories or Docker
//! containers).
//!
//! # Lifecycle
//!
//! ```text
//! provision(manifest) → SessionHandle
//!     start(handle) → Box<dyn SandboxSession>
//!         ... operations ...
//!         snapshot(handle) → SnapshotId
//!     stop(handle)
//! resume(snapshot_id) → SessionHandle
//! ```

use async_trait::async_trait;

use super::session::SandboxSession;
use super::types::{SessionHandle, SnapshotId};
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
    async fn start(&self, handle: &SessionHandle) -> Result<Box<dyn SandboxSession>, SandboxError>;

    /// Stops a running session, releasing associated resources.
    ///
    /// After stopping, the session handle is no longer valid for
    /// `start` operations. Any running child processes associated
    /// with the session are terminated.
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
}

impl std::fmt::Debug for dyn SandboxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<SandboxClient>")
    }
}
