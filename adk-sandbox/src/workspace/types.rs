//! Supporting types for the workspace lifecycle layer.
//!
//! Contains opaque handles, execution output, and directory entry types
//! used across the sandbox-agent harness.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// A command for [`SandboxSession::exec`](crate::workspace::SandboxSession::exec): the
/// shell text plus its working directory, timeout and environment.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::CommandRequest;
/// use std::time::Duration;
///
/// let request = CommandRequest::new("cargo test")
///     .working_dir("crates/core")
///     .timeout(Duration::from_secs(120))
///     .env("RUST_LOG", "debug");
/// assert_eq!(request.env["RUST_LOG"], "debug");
/// assert_eq!(request.timeout, Some(Duration::from_secs(120)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommandRequest {
    /// The shell command to run.
    pub command: String,
    /// Working directory relative to the workspace root; `None` runs in the root.
    pub working_dir: Option<String>,
    /// Longest the command may run; `None` uses the session's own command timeout.
    pub timeout: Option<Duration>,
    /// Variables set for this command only, on top of the session's environment.
    pub env: BTreeMap<String, String>,
}

impl CommandRequest {
    /// A request for `command` in the workspace root, with the session's timeout and no
    /// extra environment.
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into(), working_dir: None, timeout: None, env: BTreeMap::new() }
    }

    /// Runs the command in `dir`, relative to the workspace root.
    #[must_use]
    pub fn working_dir(mut self, dir: impl Into<String>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Ends the command after `timeout`.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets `key` to `value` for this command.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// What a [`SandboxClient`](crate::workspace::SandboxClient) supports beyond provision,
/// start, exec and stop.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::ClientCapabilities;
/// use std::time::Duration;
///
/// let caps = ClientCapabilities::default()
///     .with_snapshot(true)
///     .with_remote(true)
///     .with_network_blocked(true)
///     .with_max_lifetime(Duration::from_secs(900));
/// assert!(caps.snapshot && caps.remote && caps.network_blocked && !caps.pause);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClientCapabilities {
    /// `snapshot` and `resume` work.
    pub snapshot: bool,
    /// `pause` suspends an environment that a later `start` resumes.
    pub pause: bool,
    /// Environments run off this host, so a command cannot reach the host's files.
    pub remote: bool,
    /// The client's configuration keeps environments off the network.
    pub network_blocked: bool,
    /// The provider destroys an environment on its own once this long has passed, even
    /// when the process that created it dies; `None` when nothing outside this process
    /// bounds the lifetime.
    pub max_lifetime: Option<Duration>,
}

impl ClientCapabilities {
    /// Sets [`snapshot`](Self::snapshot).
    #[must_use]
    pub fn with_snapshot(mut self, yes: bool) -> Self {
        self.snapshot = yes;
        self
    }

    /// Sets [`pause`](Self::pause).
    #[must_use]
    pub fn with_pause(mut self, yes: bool) -> Self {
        self.pause = yes;
        self
    }

    /// Sets [`remote`](Self::remote).
    #[must_use]
    pub fn with_remote(mut self, yes: bool) -> Self {
        self.remote = yes;
        self
    }

    /// Sets [`network_blocked`](Self::network_blocked).
    #[must_use]
    pub fn with_network_blocked(mut self, yes: bool) -> Self {
        self.network_blocked = yes;
        self
    }

    /// Sets [`max_lifetime`](Self::max_lifetime).
    #[must_use]
    pub fn with_max_lifetime(mut self, lifetime: Duration) -> Self {
        self.max_lifetime = Some(lifetime);
        self
    }
}

/// Opaque handle to a provisioned sandbox session.
///
/// Returned by [`SandboxClient::provision`](crate::workspace::SandboxClient::provision) and used to reference
/// the session across lifecycle operations (start, stop, snapshot).
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::SessionHandle;
///
/// let handle = SessionHandle::new("session-abc-123");
/// assert_eq!(handle.as_str(), "session-abc-123");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionHandle(pub String);

impl SessionHandle {
    /// Creates a new `SessionHandle` from a string identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the inner string identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque identifier for a persisted workspace snapshot.
///
/// Returned by [`SandboxClient::snapshot`](crate::workspace::SandboxClient::snapshot) and accepted by
/// [`SandboxClient::resume`](crate::workspace::SandboxClient::resume) to restore a workspace to a
/// previously captured state.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::SnapshotId;
///
/// let id = SnapshotId::new("snap-2024-01-15-001");
/// assert_eq!(id.as_str(), "snap-2024-01-15-001");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    /// Creates a new `SnapshotId` from a string identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the inner string identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Result of a command execution in the sandbox.
///
/// Contains the full output of a shell command including stdout,
/// stderr, exit code, execution duration, and whether the command
/// was terminated due to a timeout.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::ExecOutput;
/// use std::time::Duration;
///
/// let output = ExecOutput::new("hello world\n", "", 0, Duration::from_millis(42), false);
/// assert_eq!(output.exit_code, 0);
/// assert!(!output.timed_out);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutput {
    /// Standard output captured from the command.
    pub stdout: String,
    /// Standard error captured from the command.
    pub stderr: String,
    /// Process exit code. Zero typically indicates success.
    pub exit_code: i32,
    /// Wall-clock duration of the command execution.
    pub duration: Duration,
    /// Whether the command was terminated due to exceeding the timeout.
    pub timed_out: bool,
}

impl ExecOutput {
    /// Creates a new `ExecOutput` with the given fields.
    pub fn new(
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        exit_code: i32,
        duration: Duration,
        timed_out: bool,
    ) -> Self {
        Self { stdout: stdout.into(), stderr: stderr.into(), exit_code, duration, timed_out }
    }
}

/// A directory entry returned by `list_dir`.
///
/// Represents a single entry in a workspace directory listing,
/// including its name and whether it is a file or directory.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::workspace::{DirEntry, EntryType};
///
/// let entry = DirEntry::new("src", EntryType::Directory);
/// assert_eq!(entry.name, "src");
/// assert_eq!(entry.entry_type, EntryType::Directory);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Name of the directory entry (file or subdirectory name, not full path).
    pub name: String,
    /// Whether this entry is a file or directory.
    #[serde(rename = "type")]
    pub entry_type: EntryType,
}

impl DirEntry {
    /// Creates a new `DirEntry` with the given name and type.
    pub fn new(name: impl Into<String>, entry_type: EntryType) -> Self {
        Self { name: name.into(), entry_type }
    }
}

/// Type of a directory entry.
///
/// Used in [`DirEntry`] to distinguish files from directories
/// in workspace directory listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_handle_equality() {
        let a = SessionHandle("session-1".to_string());
        let b = SessionHandle("session-1".to_string());
        let c = SessionHandle("session-2".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn snapshot_id_equality() {
        let a = SnapshotId("snap-1".to_string());
        let b = SnapshotId("snap-1".to_string());
        let c = SnapshotId("snap-2".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn session_handle_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(SessionHandle("a".to_string()));
        set.insert(SessionHandle("b".to_string()));
        set.insert(SessionHandle("a".to_string()));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn snapshot_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(SnapshotId("x".to_string()));
        set.insert(SnapshotId("y".to_string()));
        set.insert(SnapshotId("x".to_string()));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn exec_output_serialization_roundtrip() {
        let output = ExecOutput {
            stdout: "hello".to_string(),
            stderr: "warn".to_string(),
            exit_code: 1,
            duration: Duration::from_millis(500),
            timed_out: false,
        };
        let json = serde_json::to_string(&output).unwrap();
        let deserialized: ExecOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.stdout, "hello");
        assert_eq!(deserialized.stderr, "warn");
        assert_eq!(deserialized.exit_code, 1);
        assert_eq!(deserialized.duration, Duration::from_millis(500));
        assert!(!deserialized.timed_out);
    }

    #[test]
    fn dir_entry_serialization_roundtrip() {
        let entry = DirEntry { name: "main.rs".to_string(), entry_type: EntryType::File };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""type":"file""#));
        let deserialized: DirEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, entry);
    }

    #[test]
    fn entry_type_serialization() {
        let file_json = serde_json::to_string(&EntryType::File).unwrap();
        let dir_json = serde_json::to_string(&EntryType::Directory).unwrap();
        assert_eq!(file_json, r#""file""#);
        assert_eq!(dir_json, r#""directory""#);
    }

    #[test]
    fn session_handle_serialization_roundtrip() {
        let handle = SessionHandle("test-session".to_string());
        let json = serde_json::to_string(&handle).unwrap();
        let deserialized: SessionHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, handle);
    }

    #[test]
    fn snapshot_id_serialization_roundtrip() {
        let id = SnapshotId("test-snapshot".to_string());
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: SnapshotId = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, id);
    }
}
