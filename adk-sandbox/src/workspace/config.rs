//! Sandbox configuration types for the workspace lifecycle layer.
//!
//! Contains [`SandboxConfig`] (runtime configuration with client trait object),
//! [`SandboxConfigSpec`] (serializable subset), and [`Capability`] (tool categories).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use super::client::SandboxClient;
use super::manifest::Manifest;

/// Capabilities that can be enabled on a sandbox session.
///
/// Each capability determines which tools are bound to the agent
/// by the [`SandboxRunner`] during session setup.
///
/// # Example
///
/// ```rust
/// use std::collections::HashSet;
/// use adk_sandbox::workspace::Capability;
///
/// let mut caps = HashSet::new();
/// caps.insert(Capability::Shell);
/// caps.insert(Capability::Filesystem);
/// assert_eq!(caps.len(), 2);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Shell command execution (`exec_command` tool).
    Shell,
    /// Filesystem operations (`read_file`, `write_file`, `list_dir`, `apply_patch` tools).
    Filesystem,
}

/// Lightweight sandbox configuration attached to an LlmAgent.
///
/// Declares which client, manifest, and capabilities the `SandboxRunner`
/// should use. The client trait object is runtime-only (not serialized).
///
/// # Example
///
/// ```rust,ignore
/// use std::collections::HashSet;
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_sandbox::workspace::{Capability, Manifest, SandboxConfig};
///
/// let config = SandboxConfig {
///     client: Arc::new(my_client),
///     manifest: Manifest { entries: vec![] },
///     capabilities: HashSet::from([Capability::Shell, Capability::Filesystem]),
///     snapshot_on_stop: true,
///     session_timeout: Duration::from_secs(600),
///     command_timeout: Duration::from_secs(120),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// The sandbox client implementation (runtime-only, not serialized).
    pub client: Arc<dyn SandboxClient>,
    /// Workspace manifest defining initial contents.
    pub manifest: Manifest,
    /// Set of enabled capabilities determining which tools are bound.
    pub capabilities: HashSet<Capability>,
    /// Whether to snapshot the workspace when the session stops.
    pub snapshot_on_stop: bool,
    /// Maximum wall-clock duration for the entire sandbox session.
    pub session_timeout: Duration,
    /// Default maximum duration for individual command executions.
    pub command_timeout: Duration,
}

impl SandboxConfig {
    /// Creates a new sandbox configuration.
    ///
    /// # Arguments
    ///
    /// * `client` - The sandbox client implementation
    /// * `manifest` - Workspace manifest defining initial contents
    /// * `capabilities` - Set of enabled capabilities determining which tools are bound
    pub fn new(
        client: Arc<dyn SandboxClient>,
        manifest: Manifest,
        capabilities: HashSet<Capability>,
    ) -> Self {
        Self {
            client,
            manifest,
            capabilities,
            snapshot_on_stop: false,
            session_timeout: Duration::from_secs(600),
            command_timeout: Duration::from_secs(120),
        }
    }

    /// Sets whether to snapshot the workspace when the session stops.
    pub fn with_snapshot_on_stop(mut self, snapshot: bool) -> Self {
        self.snapshot_on_stop = snapshot;
        self
    }

    /// Sets the maximum wall-clock duration for the entire sandbox session.
    pub fn with_session_timeout(mut self, timeout: Duration) -> Self {
        self.session_timeout = timeout;
        self
    }

    /// Sets the default maximum duration for individual command executions.
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }
}

/// Serializable subset of [`SandboxConfig`] (excludes the client trait object).
///
/// Used for persisting or transmitting sandbox configuration without
/// the runtime client reference. Timeout durations are stored as seconds.
///
/// # Example
///
/// ```rust,ignore
/// use std::collections::HashSet;
/// use adk_sandbox::workspace::{Capability, Manifest, SandboxConfigSpec};
///
/// let spec = SandboxConfigSpec {
///     manifest: Manifest { entries: vec![] },
///     capabilities: HashSet::from([Capability::Shell]),
///     snapshot_on_stop: false,
///     session_timeout_secs: 600,
///     command_timeout_secs: 120,
/// };
///
/// let json = serde_json::to_string(&spec).unwrap();
/// let deserialized: SandboxConfigSpec = serde_json::from_str(&json).unwrap();
/// assert_eq!(deserialized, spec);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxConfigSpec {
    /// Workspace manifest defining initial contents.
    pub manifest: Manifest,
    /// Set of enabled capabilities determining which tools are bound.
    pub capabilities: HashSet<Capability>,
    /// Whether to snapshot the workspace when the session stops.
    pub snapshot_on_stop: bool,
    /// Maximum wall-clock duration for the entire sandbox session, in seconds.
    pub session_timeout_secs: u64,
    /// Default maximum duration for individual command executions, in seconds.
    pub command_timeout_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_equality() {
        assert_eq!(Capability::Shell, Capability::Shell);
        assert_eq!(Capability::Filesystem, Capability::Filesystem);
        assert_ne!(Capability::Shell, Capability::Filesystem);
    }

    #[test]
    fn capability_hash_set() {
        let mut set = HashSet::new();
        set.insert(Capability::Shell);
        set.insert(Capability::Filesystem);
        set.insert(Capability::Shell); // duplicate
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn capability_serialization_roundtrip() {
        let shell_json = serde_json::to_string(&Capability::Shell).unwrap();
        let fs_json = serde_json::to_string(&Capability::Filesystem).unwrap();

        let shell: Capability = serde_json::from_str(&shell_json).unwrap();
        let fs: Capability = serde_json::from_str(&fs_json).unwrap();

        assert_eq!(shell, Capability::Shell);
        assert_eq!(fs, Capability::Filesystem);
    }

    #[test]
    fn sandbox_config_spec_serialization_roundtrip() {
        let spec = SandboxConfigSpec {
            manifest: Manifest { entries: vec![] },
            capabilities: HashSet::from([Capability::Shell, Capability::Filesystem]),
            snapshot_on_stop: true,
            session_timeout_secs: 600,
            command_timeout_secs: 120,
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: SandboxConfigSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, spec);
    }

    #[test]
    fn sandbox_config_spec_empty_capabilities() {
        let spec = SandboxConfigSpec {
            manifest: Manifest { entries: vec![] },
            capabilities: HashSet::new(),
            snapshot_on_stop: false,
            session_timeout_secs: 300,
            command_timeout_secs: 60,
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: SandboxConfigSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, spec);
    }
}
