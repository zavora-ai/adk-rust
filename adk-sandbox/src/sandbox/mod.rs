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

// The restricted-token enforcer's platform-neutral surface (the policy-to-grant mapping and
// the honest probe) compiles wherever the feature is on, so it is unit-tested on every host;
// the Win32 launch is gated to Windows inside the module (spec 3.2 §10 W5).
#[cfg(any(test, feature = "sandbox-windows"))]
pub mod windows;

#[cfg(any(test, all(feature = "sandbox-linux", target_os = "linux")))]
mod seccomp;

pub(crate) mod command;

#[cfg(any(
    test,
    all(feature = "sandbox-macos", target_os = "macos"),
    all(feature = "sandbox-linux", target_os = "linux")
))]
mod layering;

pub use command::sandboxed_command;

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

/// The loopback port of an egress proxy, the one destination a
/// [`NetworkAccess::Proxied`] process may reach.
///
/// The process reaches it at `127.0.0.1:<port>` and finds it through the standard proxy
/// variables [`NetworkAccess::proxy_env`] names.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::ProxyEndpoint;
///
/// let endpoint = ProxyEndpoint { port: 47811 };
/// assert_eq!(endpoint.url(), "http://127.0.0.1:47811");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEndpoint {
    /// The loopback port.
    pub port: u16,
}

impl ProxyEndpoint {
    /// The proxy URL a process configures, `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// What a sandboxed process may reach on the network.
///
/// | Mode | The process reaches |
/// |------|---------------------|
/// | `Denied` (the default) | Nothing |
/// | `Proxied` | Only the egress proxy at [`ProxyEndpoint`], which decides every destination |
/// | `Open` | Everything the host reaches |
///
/// An enforcer that cannot enforce the mode refuses the policy with
/// [`SandboxError::Unenforceable`] instead of running it with more network access; see
/// [`SandboxEnforcer::network_support`].
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::{NetworkAccess, ProxyEndpoint};
///
/// let proxied = NetworkAccess::Proxied { endpoint: ProxyEndpoint { port: 47811 } };
/// assert!(proxied.restricts());
/// assert!(proxied.proxy_env().contains(&("HTTPS_PROXY", "http://127.0.0.1:47811".to_string())));
/// assert!(NetworkAccess::Denied.proxy_env().is_empty());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum NetworkAccess {
    /// No network access.
    #[default]
    Denied,
    /// Network access only through the egress proxy at `endpoint`.
    Proxied {
        /// Where the proxy listens.
        endpoint: ProxyEndpoint,
    },
    /// Unrestricted network access.
    Open,
}

impl NetworkAccess {
    /// Whether the mode restricts the network (`Denied` or `Proxied`).
    pub fn restricts(&self) -> bool {
        !matches!(self, Self::Open)
    }

    /// The variables a `Proxied` process runs with, upper and lower case: `HTTP_PROXY`,
    /// `HTTPS_PROXY` and `ALL_PROXY` name the endpoint, and `NO_PROXY` is empty so no
    /// destination bypasses it. Empty for the other modes.
    pub fn proxy_env(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Proxied { endpoint } => {
                let url = endpoint.url();
                let mut env: Vec<(&'static str, String)> = [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                ]
                .into_iter()
                .map(|name| (name, url.clone()))
                .collect();
                env.push(("NO_PROXY", String::new()));
                env.push(("no_proxy", String::new()));
                env
            }
            Self::Denied | Self::Open => Vec::new(),
        }
    }
}

impl std::fmt::Display for NetworkAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => f.write_str("denied"),
            Self::Proxied { endpoint } => write!(f, "proxied through 127.0.0.1:{}", endpoint.port),
            Self::Open => f.write_str("open"),
        }
    }
}

/// Which restrictive [`NetworkAccess`] modes an enforcer enforces; `Open` needs no enforcement.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::{NetworkAccess, NetworkSupport};
///
/// let support = NetworkSupport { denied: true, proxied: false };
/// assert!(support.enforces(&NetworkAccess::Denied));
/// assert!(support.enforces(&NetworkAccess::Open));
/// assert!(NetworkSupport::NONE.check("custom", &NetworkAccess::Denied).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSupport {
    /// Whether `Denied` is enforced.
    pub denied: bool,
    /// Whether `Proxied` is enforced.
    pub proxied: bool,
}

impl NetworkSupport {
    /// No network restriction is enforced.
    pub const NONE: Self = Self { denied: false, proxied: false };

    /// Whether `network` is enforced.
    pub fn enforces(&self, network: &NetworkAccess) -> bool {
        match network {
            NetworkAccess::Denied => self.denied,
            NetworkAccess::Proxied { .. } => self.proxied,
            NetworkAccess::Open => true,
        }
    }

    /// `Ok` when `network` is enforced.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Unenforceable`] naming `enforcer` when `network` is not enforced.
    pub fn check(&self, enforcer: &str, network: &NetworkAccess) -> Result<(), SandboxError> {
        if self.enforces(network) {
            return Ok(());
        }
        Err(SandboxError::Unenforceable {
            enforcer: enforcer.to_string(),
            requirement: format!("network access {network}"),
        })
    }
}

/// A declarative sandbox policy describing allowed operations.
///
/// Constructed via [`SandboxPolicyBuilder`]. Defaults to deny-all when
/// no permissions are granted.
///
/// # Network Access
///
/// [`network`](Self::network) is one of three modes, and an enforcer runs a policy only when
/// it enforces the mode; otherwise it returns [`SandboxError::Unenforceable`]:
///
/// | Enforcer | `Denied` | `Proxied` | `Open` |
/// |----------|----------|-----------|--------|
/// | Seatbelt (macOS) | `(deny network*)`, Unix-domain sockets included | Only `localhost:<port>` of the proxy | Allowed |
/// | bubblewrap (Linux) | `--unshare-net`: no IP traffic and no abstract Unix sockets; Unix sockets on the filesystem under granted paths stay reachable, so `/` and `/run` cannot be granted | Refused | Allowed |
/// | Restricted token (Windows) | Refused, unless the caller acknowledges that the network stays open (`WindowsEnforcer::acknowledge_open_network`) | Refused the same way | Allowed |
///
/// Policies serialized before 3.0 load: `allowNetwork: true` becomes `Open`, `false` becomes
/// `Denied`, and `networkRules` is dropped, as no enforcer ever applied it.
///
/// # Example
///
/// ```rust
/// use adk_sandbox::sandbox::{NetworkAccess, SandboxPolicy, SandboxPolicyBuilder};
///
/// let policy = SandboxPolicyBuilder::new().allow_read("/usr/lib").env("PATH", "/usr/bin").build();
/// assert_eq!(policy.network, NetworkAccess::Denied);
///
/// let legacy: SandboxPolicy = serde_json::from_str(
///     r#"{"allowedPaths":[],"allowNetwork":true,"allowProcessSpawn":false,"env":{}}"#,
/// )
/// .unwrap();
/// assert_eq!(legacy.network, NetworkAccess::Open);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "SandboxPolicyWire")]
pub struct SandboxPolicy {
    /// Filesystem paths the process may access. Each path must exist; enforcers resolve
    /// them and fail with `PolicyViolation` otherwise.
    ///
    /// When entries nest, the most specific one decides, whatever their order (spec 3.1
    /// §10 S3). A `ReadOnly` entry inside a `ReadWrite` entry — `.git` inside a workspace —
    /// refuses every write, creation, removal, rename, and link beneath it on Seatbelt and
    /// bubblewrap alike, and the writable directories above it cannot be renamed or removed.
    /// For the same path, `ReadWrite` wins.
    pub allowed_paths: Vec<AllowedPath>,
    /// What the process may reach on the network; see [Network Access](Self#network-access).
    pub network: NetworkAccess,
    /// Whether the process may create child processes.
    ///
    /// When `false`, replacing the process image with `execve` remains allowed
    /// but creating a new process is refused:
    ///
    /// | Enforcer | Mechanism |
    /// |----------|-----------|
    /// | Seatbelt (macOS) | `process-fork` is not granted |
    /// | bubblewrap (Linux) | seccomp filter failing `fork`, `vfork`, and non-thread `clone` (x86-64 and AArch64) |
    /// | Restricted token (Windows) | Not restricted |
    ///
    /// A shell running more than one external command forks, so `sh -c` scripts
    /// usually need this enabled.
    pub allow_process_spawn: bool,
    /// Environment variables passed to the sandboxed process.
    pub env: HashMap<String, String>,
}

/// The serialized form of [`SandboxPolicy`], which also reads the fields it had before 3.0.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxPolicyWire {
    #[serde(default)]
    allowed_paths: Vec<AllowedPath>,
    #[serde(default)]
    network: Option<NetworkAccess>,
    /// Before 3.0: `true` for all network access.
    #[serde(default)]
    allow_network: Option<bool>,
    /// Before 3.0: domain rules no enforcer applied; read and dropped.
    #[serde(default, rename = "networkRules")]
    _network_rules: serde::de::IgnoredAny,
    #[serde(default)]
    allow_process_spawn: bool,
    #[serde(default)]
    env: HashMap<String, String>,
}

impl From<SandboxPolicyWire> for SandboxPolicy {
    fn from(wire: SandboxPolicyWire) -> Self {
        let legacy = wire
            .allow_network
            .map(|open| if open { NetworkAccess::Open } else { NetworkAccess::Denied });
        Self {
            allowed_paths: wire.allowed_paths,
            network: wire.network.or(legacy).unwrap_or_default(),
            allow_process_spawn: wire.allow_process_spawn,
            env: wire.env,
        }
    }
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
/// assert_eq!(policy.network, adk_sandbox::sandbox::NetworkAccess::Open);
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
                network: NetworkAccess::Denied,
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

    /// Sets what the process may reach on the network.
    pub fn network(mut self, network: NetworkAccess) -> Self {
        self.policy.network = network;
        self
    }

    /// Enables full network access: [`NetworkAccess::Open`].
    pub fn allow_network(self) -> Self {
        self.network(NetworkAccess::Open)
    }

    /// Lets the process reach only the egress proxy at `endpoint`: [`NetworkAccess::Proxied`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_sandbox::sandbox::{NetworkAccess, ProxyEndpoint, SandboxPolicyBuilder};
    ///
    /// let endpoint = ProxyEndpoint { port: 47811 };
    /// let policy = SandboxPolicyBuilder::new().proxied(endpoint).build();
    /// assert_eq!(policy.network, NetworkAccess::Proxied { endpoint });
    /// ```
    pub fn proxied(self, endpoint: ProxyEndpoint) -> Self {
        self.network(NetworkAccess::Proxied { endpoint })
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
/// # Building a Command
///
/// [`sandboxed_command`] calls `wrap_command()` to obtain the wrapper program
/// and args, constructs a new `Command` with those values, and then calls
/// `configure_command()` on it. `ProcessBackend` builds its sandboxed commands
/// the same way. This avoids the limitation that tokio's Command doesn't expose
/// `get_program()`/`get_args()` setters after creation.
///
/// # Windows Exception
///
/// On Windows, `WindowsEnforcer` confines a command with a restricted token, which
/// is set when the process is created and so cannot be attached to a
/// `tokio::process::Command`. Its `wrap_command` returns the original program and
/// args unchanged, and its `probe` and `configure_command` fail, naming
/// `WindowsEnforcer::run_confined` as the path that confines (spec 3.2 §10 W5).
pub trait SandboxEnforcer: Send + Sync {
    /// Returns the enforcer name (e.g., "seatbelt", "bubblewrap", "restricted-token").
    fn name(&self) -> &str;

    /// Checks whether the enforcer is functional on the current system.
    fn probe(&self) -> Result<(), SandboxError>;

    /// The restrictive [`NetworkAccess`] modes this enforcer enforces.
    ///
    /// [`wrap_command`](Self::wrap_command) refuses a policy whose mode is not among them with
    /// [`SandboxError::Unenforceable`]; it never runs a command with more network access than
    /// the policy asks for.
    fn network_support(&self) -> NetworkSupport;

    /// Wraps the original command with sandbox enforcement.
    ///
    /// Given the original program and its arguments, returns a [`WrappedCommand`]
    /// containing the sandbox wrapper program and the full argument list.
    ///
    /// This method:
    /// 1. Returns `SandboxError::Unenforceable` if the policy's network mode is not enforced
    /// 2. Canonicalizes all paths in the policy (logs `tracing::warn` if changed)
    /// 3. Returns `SandboxError::PolicyViolation` if any path cannot be resolved
    /// 4. Generates the platform-specific wrapper (Seatbelt profile, bwrap args, etc.)
    /// 5. Returns the wrapped program and args
    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError>;

    /// Configures the Command with platform-specific process attributes.
    ///
    /// Must be called on the Command constructed from `wrap_command()` output,
    /// after its program and arguments are final. The default implementation is
    /// a no-op. Linux uses it to install the process-creation seccomp program
    /// that the wrapped arguments reference; the Windows enforcer fails here, since a
    /// restricted token cannot be set on a `tokio::process::Command`.
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
