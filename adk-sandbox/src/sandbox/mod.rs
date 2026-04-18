//! OS-level sandbox enforcement types and traits.
//!
//! This module defines the platform-agnostic [`SandboxPolicy`] data model,
//! the [`SandboxEnforcer`] trait for platform-specific enforcement, and the
//! [`get_enforcer`] registry function that selects the appropriate enforcer
//! for the current platform.

#[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
pub mod macos;

#[cfg(all(feature = "sandbox-linux", target_os = "linux"))]
pub mod linux;

#[cfg(all(feature = "sandbox-windows", target_os = "windows"))]
pub mod windows;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;

/// Filesystem access mode for an allowed path.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::AccessMode;
///
/// let mode = AccessMode::ReadOnly;
/// assert_ne!(mode, AccessMode::ReadWrite);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessMode {
    /// Read-only access.
    ReadOnly,
    /// Read and write access.
    ReadWrite,
}

/// A filesystem path entry with an access mode.
///
/// # Example
///
/// ```rust
/// use std::path::PathBuf;
/// use adk_sandbox::sandbox::{AllowedPath, AccessMode};
///
/// let entry = AllowedPath {
///     path: PathBuf::from("/tmp"),
///     mode: AccessMode::ReadOnly,
/// };
/// assert_eq!(entry.mode, AccessMode::ReadOnly);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedPath {
    /// The filesystem path (directory or file).
    pub path: PathBuf,
    /// The access mode: read-only or read-write.
    pub mode: AccessMode,
}

/// A declarative sandbox policy describing allowed operations.
///
/// Constructed via [`SandboxPolicyBuilder`]. Defaults to deny-all when
/// no permissions are granted.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::SandboxPolicyBuilder;
///
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr/lib")
///     .allow_read_write("/tmp/work")
///     .allow_network()
///     .env("PATH", "/usr/bin")
///     .build();
///
/// assert!(policy.allow_network);
/// assert!(!policy.allow_process_spawn);
/// assert_eq!(policy.allowed_paths.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxPolicy {
    /// Filesystem paths the process may access.
    pub allowed_paths: Vec<AllowedPath>,
    /// Whether the process may access the network.
    pub allow_network: bool,
    /// Whether the process may spawn child processes.
    pub allow_process_spawn: bool,
    /// Environment variables passed to the sandboxed process.
    pub env: HashMap<String, String>,
}

/// The result of wrapping a command with sandbox enforcement.
///
/// Contains the new program to execute and the full argument list
/// (sandbox wrapper args + original program + original args).
#[derive(Debug, Clone)]
pub struct WrappedCommand {
    /// The program to execute (e.g., "sandbox-exec", "bwrap", or the original program for Windows).
    pub program: OsString,
    /// The full argument list including wrapper args, separator, and original args.
    pub args: Vec<OsString>,
}

/// Builder for constructing [`SandboxPolicy`] values incrementally.
///
/// Defaults to deny-all: no allowed paths, no network, no process spawning,
/// and no environment variables.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::SandboxPolicyBuilder;
///
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr/lib")
///     .allow_read_write("/tmp/work")
///     .allow_network()
///     .allow_process_spawn()
///     .env("HOME", "/home/user")
///     .build();
///
/// assert_eq!(policy.allowed_paths.len(), 2);
/// assert!(policy.allow_network);
/// assert!(policy.allow_process_spawn);
/// assert_eq!(policy.env.get("HOME").unwrap(), "/home/user");
/// ```
pub struct SandboxPolicyBuilder {
    policy: SandboxPolicy,
}

impl SandboxPolicyBuilder {
    /// Creates a new builder with deny-all defaults.
    pub fn new() -> Self {
        Self {
            policy: SandboxPolicy {
                allowed_paths: Vec::new(),
                allow_network: false,
                allow_process_spawn: false,
                env: HashMap::new(),
            },
        }
    }

    /// Adds a read-only allowed path.
    pub fn allow_read(mut self, path: impl Into<PathBuf>) -> Self {
        self.policy
            .allowed_paths
            .push(AllowedPath { path: path.into(), mode: AccessMode::ReadOnly });
        self
    }

    /// Adds a read-write allowed path.
    pub fn allow_read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.policy
            .allowed_paths
            .push(AllowedPath { path: path.into(), mode: AccessMode::ReadWrite });
        self
    }

    /// Enables network access.
    pub fn allow_network(mut self) -> Self {
        self.policy.allow_network = true;
        self
    }

    /// Enables child process spawning.
    pub fn allow_process_spawn(mut self) -> Self {
        self.policy.allow_process_spawn = true;
        self
    }

    /// Adds an environment variable key-value pair.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.policy.env.insert(key.into(), value.into());
        self
    }

    /// Consumes the builder and returns the constructed [`SandboxPolicy`].
    pub fn build(self) -> SandboxPolicy {
        self.policy
    }
}

impl Default for SandboxPolicyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform-specific sandbox enforcement.
///
/// Implementations translate a [`SandboxPolicy`] into OS-native restrictions.
/// The trait uses a `wrap_command` approach rather than mutating a `Command`
/// directly, because `tokio::process::Command` does not allow replacing the
/// program after construction.
///
/// # Integration with ProcessBackend
///
/// `ProcessBackend::run_command()` calls `wrap_command()` to obtain the
/// wrapper program and args, then constructs a new `Command` with those
/// values. This avoids the limitation that tokio's Command doesn't expose
/// `get_program()`/`get_args()` setters after creation.
///
/// # Windows Exception
///
/// On Windows, `WindowsEnforcer` does NOT wrap the command — it configures
/// the process token via Win32 APIs. Its `wrap_command` returns the original
/// program and args unchanged, and `configure_command` applies the
/// AppContainer restrictions via `Command::creation_flags()` and
/// pre-spawn setup.
pub trait SandboxEnforcer: Send + Sync {
    /// Returns the enforcer name (e.g., "seatbelt", "bubblewrap", "appcontainer").
    fn name(&self) -> &str;

    /// Checks whether the enforcer is functional on the current system.
    fn probe(&self) -> Result<(), SandboxError>;

    /// Wraps the original command with sandbox enforcement.
    ///
    /// Given the original program and its arguments, returns a [`WrappedCommand`]
    /// containing the sandbox wrapper program and the full argument list.
    ///
    /// This method:
    /// 1. Canonicalizes all paths in the policy (logs `tracing::warn` if changed)
    /// 2. Returns `SandboxError::PolicyViolation` if any path cannot be resolved
    /// 3. Generates the platform-specific wrapper (Seatbelt profile, bwrap args, etc.)
    /// 4. Returns the wrapped program and args
    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError>;

    /// Optional: configure the Command with platform-specific process attributes.
    ///
    /// Called after the Command is constructed from `wrap_command()` output.
    /// Default implementation is a no-op. Windows uses this to set
    /// AppContainer process attributes via `creation_flags()` and
    /// `raw_attribute()`.
    fn configure_command(
        &self,
        _cmd: &mut tokio::process::Command,
        _policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        Ok(())
    }
}

/// Returns the platform-appropriate sandbox enforcer.
///
/// Selects the enforcer based on enabled feature flags, then calls `probe()`
/// to verify it is functional. Returns an error if no enforcer is available
/// or if the probe fails.
///
/// # Errors
///
/// Returns `SandboxError::EnforcerUnavailable` if no sandbox feature flag is
/// enabled for the current platform, or if the selected enforcer's `probe()`
/// check fails.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::sandbox::get_enforcer;
///
/// let enforcer = get_enforcer()?;
/// println!("Using enforcer: {}", enforcer.name());
/// ```
pub fn get_enforcer() -> Result<Box<dyn SandboxEnforcer>, SandboxError> {
    #[cfg(all(feature = "sandbox-macos", target_os = "macos"))]
    {
        let enforcer = macos::MacOsEnforcer::new();
        enforcer.probe()?;
        return Ok(Box::new(enforcer));
    }

    #[cfg(all(feature = "sandbox-linux", target_os = "linux"))]
    {
        let enforcer = linux::LinuxEnforcer::new();
        enforcer.probe()?;
        return Ok(Box::new(enforcer));
    }

    #[cfg(all(feature = "sandbox-windows", target_os = "windows"))]
    {
        let enforcer = windows::WindowsEnforcer::new();
        enforcer.probe()?;
        return Ok(Box::new(enforcer));
    }

    #[allow(unreachable_code)]
    Err(SandboxError::EnforcerUnavailable {
        enforcer: "none".to_string(),
        message: "no sandbox feature flag is enabled for this platform".to_string(),
    })
}
