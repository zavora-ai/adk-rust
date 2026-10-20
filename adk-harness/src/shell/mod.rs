//! Shell isolation and the per-session shell ledger (spec 3.1 §7.9).
//!
//! The shell follows the pattern Claude Code uses: the OS sandbox is off by default,
//! host commands run behind per-command approval, the sandbox is opt-in, and a strict
//! deployment can fail at startup when it is unavailable.

mod ledger;
#[cfg(feature = "sandbox")]
mod os_sandbox;
mod protect;

use std::fmt;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use serde::{Deserialize, Serialize};

#[cfg(feature = "sqlite")]
pub use ledger::SqliteShellLedger;
pub use ledger::{InMemoryShellLedger, ShellLedger, ShellLedgerEntry};
#[cfg(feature = "sandbox")]
pub use os_sandbox::{OsSandboxConfig, SandboxAccess, SandboxLauncher};
pub use protect::SANDBOX_PROTECTED_PATHS;
pub(crate) use protect::protected_paths;

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
    /// Run sandboxed commands without approval in Ask (spec 3.1 §9.2 I26). Turns on only
    /// when the enforcer passes the nested read-only conformance probe (adk-sandbox S3),
    /// the sandbox denies the network and every root-level protected path exists; the
    /// build fails otherwise, naming the reason. No effect on the host or a degraded shell.
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

/// Error code of an `auto_allow_sandboxed` the sandbox cannot honour (spec 3.1 §7.9, I26).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::AUTO_ALLOW_UNAVAILABLE_CODE, "harness.auto_allow_sandboxed_unavailable");
/// ```
pub const AUTO_ALLOW_UNAVAILABLE_CODE: &str = "harness.auto_allow_sandboxed_unavailable";

/// The shell as `build()` resolved it (spec 3.1 §7.2 step 1).
#[derive(Debug, Clone)]
pub(crate) struct ShellRuntime {
    /// The isolation `bash` runs under.
    pub(crate) status: IsolationStatus,
    /// The launcher of an active OS sandbox.
    #[cfg(feature = "sandbox")]
    pub(crate) launcher: Option<SandboxLauncher>,
    /// The sandbox denies the network, keeps protected paths read-only (adk-sandbox S3, proven
    /// by the enforcer conformance probe) and every root-level protected path exists, so
    /// sandboxed commands may run without approval where the mode table allows it (I26).
    pub(crate) protects_paths: bool,
    /// `auto_allow_sandboxed` is in effect.
    pub(crate) auto_allow_sandboxed: bool,
}

/// Resolves the shell runtime at build time (spec 3.1 §7.2 step 1, §7.9 "Availability").
///
/// Probes the enforcer of a requested OS sandbox. An unavailable one fails the build under
/// `fail_if_unavailable` and is otherwise `Degraded`: the host shell behind approvals, with
/// `auto_allow_sandboxed` of no effect. An active one is checked for S3 semantics; an
/// `auto_allow_sandboxed` it cannot honour fails the build with the reason.
pub(crate) fn resolve_isolation(
    config: &ShellConfig,
    root: &std::path::Path,
) -> Result<ShellRuntime> {
    #[cfg(not(feature = "sandbox"))]
    let _ = root;
    let runtime = match &config.isolation {
        ShellIsolation::Host => ShellRuntime {
            status: IsolationStatus::Host,
            #[cfg(feature = "sandbox")]
            launcher: None,
            protects_paths: false,
            auto_allow_sandboxed: false,
        },
        #[cfg(feature = "sandbox")]
        ShellIsolation::OsSandbox(sandbox) => {
            let launcher = SandboxLauncher::new(sandbox.clone());
            let status = launcher.status();
            let active = matches!(status, IsolationStatus::OsSandbox { .. });
            let refusal = if active { auto_allow_refusal(&launcher, root) } else { None };
            if config.auto_allow_sandboxed
                && let Some(reason) = &refusal
            {
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::InvalidInput,
                    AUTO_ALLOW_UNAVAILABLE_CODE,
                    format!(
                        "auto_allow_sandboxed cannot turn on: {reason} (spec 3.1 §9.2 I26); \
                         drop auto_allow_sandboxed to approve sandboxed commands one by one"
                    ),
                ));
            }
            ShellRuntime {
                protects_paths: active && refusal.is_none(),
                auto_allow_sandboxed: config.auto_allow_sandboxed && active && refusal.is_none(),
                launcher: active.then_some(launcher),
                status,
            }
        }
    };
    if let IsolationStatus::Degraded { requested, reason } = &runtime.status {
        if config.fail_if_unavailable {
            return Err(sandbox_unavailable(requested, reason));
        }
        tracing::warn!(
            shell.requested = %requested,
            shell.reason = %reason,
            "requested shell sandbox is unavailable; bash runs on the host and every command needs approval in ask mode"
        );
    }
    if config.auto_allow_sandboxed && !runtime.auto_allow_sandboxed {
        tracing::warn!(
            shell.isolation = %runtime.status,
            "auto_allow_sandboxed has no effect: no OS sandbox is active"
        );
    }
    Ok(runtime)
}

/// Why sandboxed commands cannot run unapproved, or `None` when they can (I26).
#[cfg(feature = "sandbox")]
fn auto_allow_refusal(launcher: &SandboxLauncher, root: &std::path::Path) -> Option<String> {
    if launcher.config().network {
        return Some("the sandbox allows the network".to_string());
    }
    if let Err(reason) = launcher.nested_read_only_conformance() {
        return Some(reason);
    }
    let protected = protected_paths(root);
    (!protected.unprotectable.is_empty()).then(|| {
        format!(
            "these protected paths cannot be kept read-only inside the sandbox: {}; create them \
             (a protected path that does not exist cannot be protected from creation) or \
             replace the symlinks",
            protected.describe_unprotectable()
        )
    })
}

/// Whether every root-level protected path of `root` still exists and can be kept
/// read-only, re-checked per call while sandboxed commands run unapproved (I26).
pub(crate) fn protected_paths_hold(root: &std::path::Path) -> bool {
    protected_paths(root).unprotectable.is_empty()
}

/// Records `harness.bash.retry_unsandboxed` for an unsandboxed retry the policy decided
/// (spec 3.1 §9.3, `source: "policy"`); the tool records the consumption.
pub(crate) async fn record_retry_requested(
    audit: Option<crate::audit::HarnessAudit>,
    ledger: &dyn ShellLedger,
    request: &adk_core::ToolPolicyRequest,
    call_id: Option<&str>,
    decision: &adk_core::PolicyDecision,
) {
    let Some(audit) = audit else { return };
    let retry_of = request.args.get("retry_of").and_then(serde_json::Value::as_str);
    let failed = match retry_of {
        Some(retry_of) => ledger
            .get(&request.app_name, &request.user_id, &request.session_id, retry_of)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let decision = match decision {
        adk_core::PolicyDecision::Allow => "allow",
        adk_core::PolicyDecision::Deny { .. } => "deny",
        adk_core::PolicyDecision::RequireApproval { .. } => "require_approval",
    };
    let record = audit
        .record(
            "harness.bash.retry_unsandboxed",
            serde_json::json!({
                "functionCallId": call_id,
                "retryOf": retry_of,
                "failedEnforcer": failed.as_ref().and_then(|entry| entry.enforcer.clone()),
                "failureDigest": failed.as_ref().map(ShellLedgerEntry::digest),
                "decision": decision,
                "source": "policy",
            }),
        )
        .await;
    if let Err(error) = record {
        tracing::warn!(error = %error, "could not record harness.bash.retry_unsandboxed");
    }
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
