//! The OS-sandbox shell launcher (spec 3.1 §7.9, feature `sandbox`).
//!
//! Running `bash` under the OS sandbox needs `adk_sandbox::sandbox::sandboxed_command`
//! (spec 3.1 §10 S1) and the devtools `CommandLauncher` (D1). Until both land, the
//! launcher reports [`IsolationStatus::Degraded`] and the shell runs on the host behind
//! approvals, or the build fails when `fail_if_unavailable` is set.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use adk_sandbox::sandbox::SandboxEnforcer;

use super::IsolationStatus;

/// Configuration of the OS-sandbox shell (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::OsSandboxConfig;
///
/// let config = OsSandboxConfig::default();
/// assert!(!config.network);
/// assert!(config.allow_process_spawn);
/// assert!(config.read_toolchains);
/// ```
#[derive(Clone)]
pub struct OsSandboxConfig {
    /// Allow network access. `false`: adk-sandbox cannot filter domains.
    pub network: bool,
    /// Allow the sandboxed shell to fork. `true`: `sh -c` forks.
    pub allow_process_spawn: bool,
    /// Extra read-only paths.
    pub extra_read: Vec<PathBuf>,
    /// Extra read-write paths.
    pub extra_write: Vec<PathBuf>,
    /// Read-only bind of the toolchain directories (`PATH`, `~/.cargo`, `~/.rustup`, …).
    pub read_toolchains: bool,
    /// The enforcer; `None` uses `adk_sandbox::sandbox::get_enforcer()`.
    pub enforcer: Option<Arc<dyn SandboxEnforcer>>,
}

impl Default for OsSandboxConfig {
    fn default() -> Self {
        Self {
            network: false,
            allow_process_spawn: true,
            extra_read: Vec::new(),
            extra_write: Vec::new(),
            read_toolchains: true,
            enforcer: None,
        }
    }
}

impl fmt::Debug for OsSandboxConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsSandboxConfig")
            .field("network", &self.network)
            .field("allow_process_spawn", &self.allow_process_spawn)
            .field("extra_read", &self.extra_read)
            .field("extra_write", &self.extra_write)
            .field("read_toolchains", &self.read_toolchains)
            .field("enforcer", &self.enforcer.as_ref().map(|enforcer| enforcer.name().to_string()))
            .finish()
    }
}

/// Builds sandboxed shell commands (spec 3.1 §7.9).
///
/// A documented stub until adk-sandbox S1 and adk-devtools D1 merge (spec 3.1 §10): it
/// probes the enforcer for the reason it reports and always yields
/// [`IsolationStatus::Degraded`].
///
/// # Example
///
/// ```rust
/// use adk_harness::{IsolationStatus, OsSandboxConfig, SandboxLauncher};
///
/// let status = SandboxLauncher::new(OsSandboxConfig::default()).status();
/// assert!(matches!(status, IsolationStatus::Degraded { .. }));
/// ```
#[derive(Debug, Clone)]
pub struct SandboxLauncher {
    config: OsSandboxConfig,
}

impl SandboxLauncher {
    /// A launcher for `config`.
    pub fn new(config: OsSandboxConfig) -> Self {
        Self { config }
    }

    /// The isolation this launcher provides.
    pub fn status(&self) -> IsolationStatus {
        probe(&self.config)
    }
}

/// Probes the configured enforcer and reports the shell as degraded until the sandboxed
/// launcher exists.
pub(super) fn probe(config: &OsSandboxConfig) -> IsolationStatus {
    let enforcer = match &config.enforcer {
        Some(enforcer) => enforcer.probe().map(|()| enforcer.name().to_string()),
        None => adk_sandbox::sandbox::get_enforcer().map(|enforcer| enforcer.name().to_string()),
    };
    let reason = match enforcer {
        Ok(name) => format!(
            "the {name} enforcer is available, but sandboxed bash needs \
             adk_sandbox::sandbox::sandboxed_command and the devtools CommandLauncher \
             (spec 3.1 §10 S1, D1)"
        ),
        Err(error) => format!("no sandbox enforcer is available: {error}"),
    };
    IsolationStatus::Degraded { requested: "os_sandbox".to_string(), reason }
}
