//! Shell isolation and the per-session shell ledger (spec 3.1 §7.9).
//!
//! The shell follows the pattern Claude Code uses: the OS sandbox is off by default,
//! host commands run behind per-command approval, the sandbox is opt-in, and a strict
//! deployment can fail at startup when it is unavailable.

mod ledger;
#[cfg(feature = "sandbox")]
mod os_sandbox;

use std::fmt;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use serde::{Deserialize, Serialize};

#[cfg(feature = "sqlite")]
pub use ledger::SqliteShellLedger;
pub use ledger::{InMemoryShellLedger, ShellLedger, ShellLedgerEntry};
#[cfg(feature = "sandbox")]
pub use os_sandbox::{OsSandboxConfig, SandboxLauncher};

use crate::policy::Isolation;

/// Error code of a requested sandbox that is not available (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::SANDBOX_UNAVAILABLE_CODE, "harness.sandbox_unavailable");
/// ```
pub const SANDBOX_UNAVAILABLE_CODE: &str = "harness.sandbox_unavailable";

/// Where `bash` runs (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellIsolation;
///
/// assert!(matches!(ShellIsolation::default(), ShellIsolation::Host));
/// ```
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ShellIsolation {
    /// The devtools `bash` on the host, governed per call. The default.
    #[default]
    Host,
    /// The OS sandbox (Seatbelt on macOS, bubblewrap on Linux).
    #[cfg(feature = "sandbox")]
    OsSandbox(OsSandboxConfig),
}

impl ShellIsolation {
    /// The requested isolation's name, as audit records and `IsolationStatus::Degraded`
    /// report it.
    pub fn requested_name(&self) -> &'static str {
        match self {
            Self::Host => "host",
            #[cfg(feature = "sandbox")]
            Self::OsSandbox(_) => "os_sandbox",
        }
    }
}

/// How the harness shell is configured (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellConfig;
///
/// let config = ShellConfig::default();
/// assert!(!config.fail_if_unavailable);
/// assert!(config.allow_unsandboxed_retry);
/// ```
#[derive(Clone, Debug)]
pub struct ShellConfig {
    /// Where commands run.
    pub isolation: ShellIsolation,
    /// Refuse to build, and refuse each call, when the requested sandbox is unavailable.
    pub fail_if_unavailable: bool,
    /// Run sandboxed commands without approval in Ask. Refuses to turn on until
    /// protected paths are read-only inside the sandbox (adk-sandbox S3).
    pub auto_allow_sandboxed: bool,
    /// Offer an unsandboxed retry of a failed sandboxed command, behind a one-time
    /// approval. `true` for host and OS sandbox, `false` for containers.
    pub allow_unsandboxed_retry: bool,
    /// Run host commands without approval in Auto.
    pub allow_host_shell_in_auto: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            isolation: ShellIsolation::Host,
            fail_if_unavailable: false,
            auto_allow_sandboxed: false,
            allow_unsandboxed_retry: true,
            allow_host_shell_in_auto: false,
        }
    }
}

/// The isolation `bash` actually runs under (spec 3.1 §7.9).
///
/// # Example
///
/// ```rust
/// use adk_harness::IsolationStatus;
///
/// let degraded = IsolationStatus::Degraded { requested: "os_sandbox".into(), reason: "no bwrap".into() };
/// assert!(degraded.behaves_as_host());
/// assert_eq!(degraded.enforcer(), "host");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IsolationStatus {
    /// The host shell.
    Host,
    /// An active OS sandbox enforcer.
    OsSandbox {
        /// The enforcer's name (`seatbelt`, `bubblewrap`).
        enforcer: String,
    },
    /// A container.
    Container {
        /// The container image.
        image: String,
    },
    /// A sandbox was requested and is unavailable; commands run on the host.
    Degraded {
        /// The requested isolation.
        requested: String,
        /// Why it is unavailable.
        reason: String,
    },
}

impl IsolationStatus {
    /// Whether commands run on the host.
    pub fn behaves_as_host(&self) -> bool {
        matches!(self, Self::Host | Self::Degraded { .. })
    }

    /// The enforcer name recorded in audit records and annotations.
    pub fn enforcer(&self) -> &str {
        match self {
            Self::Host | Self::Degraded { .. } => "host",
            Self::OsSandbox { enforcer } => enforcer,
            Self::Container { .. } => "container",
        }
    }

    /// The policy's isolation class for this status.
    pub fn isolation(&self) -> Isolation {
        match self {
            Self::Host | Self::Degraded { .. } => Isolation::Host,
            Self::OsSandbox { .. } => Isolation::OsSandbox,
            Self::Container { .. } => Isolation::Container,
        }
    }
}

impl fmt::Display for IsolationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => f.write_str("host (full user permissions, network)"),
            Self::OsSandbox { enforcer } => write!(f, "OS sandbox ({enforcer}, no network)"),
            Self::Container { image } => write!(f, "container ({image})"),
            Self::Degraded { requested, reason } => write!(
                f,
                "host (full user permissions, network); requested {requested} is unavailable: {reason}"
            ),
        }
    }
}

/// Resolves the shell runtime at build time (spec 3.1 §7.2 step 1, §7.9 "Availability").
pub(crate) fn resolve_isolation(config: &ShellConfig) -> Result<IsolationStatus> {
    if config.auto_allow_sandboxed {
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::InvalidInput,
            "harness.auto_allow_sandboxed_unavailable",
            "auto_allow_sandboxed needs protected paths to be read-only inside the sandbox \
             (adk-sandbox S3, spec 3.1 §10); it refuses to turn on until then",
        ));
    }
    let status = match &config.isolation {
        ShellIsolation::Host => IsolationStatus::Host,
        #[cfg(feature = "sandbox")]
        ShellIsolation::OsSandbox(sandbox) => os_sandbox::probe(sandbox),
    };
    if let IsolationStatus::Degraded { requested, reason } = &status {
        if config.fail_if_unavailable {
            return Err(sandbox_unavailable(requested, reason));
        }
        tracing::warn!(
            shell.requested = %requested,
            shell.reason = %reason,
            "requested shell sandbox is unavailable; bash runs on the host and every command needs approval in ask mode"
        );
    }
    Ok(status)
}

pub(crate) fn sandbox_unavailable(requested: &str, reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        SANDBOX_UNAVAILABLE_CODE,
        format!(
            "the requested {requested} shell is unavailable ({reason}) and fail_if_unavailable \
             is set; install the sandbox or drop fail_if_unavailable to run on the host behind approvals"
        ),
    )
}
