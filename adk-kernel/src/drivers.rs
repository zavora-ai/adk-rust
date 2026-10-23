//! Drivers: MCP servers run as supervised child processes under a manifest the user
//! approved (spec 3.2 §7.7).
//!
//! | Item | Spec |
//! |---|---|
//! | [`EffectClass`], [`DriverState`] | §7.7, §7.6 |
//! | [`DriverManifest`], [`DriverSandbox`], [`DriverLifecycle`], [`DriverAccess`], [`load_manifests`] (feature `drivers`) | §7.7 |
//! | [`DriverStore`], [`DriverRecord`], [`DriverPark`], [`DriverIsolation`] (feature `drivers`) | §7.7, §9.3, §13 Q8, Q9 |
//! | [`DriverSupervisor`], [`DriverSupervisorBuilder`], [`DriverProposal`], [`DriverView`], [`StandingRulePins`] (feature `drivers`) | §7.7, K-I13, K-I14 |
//! | [`DriverPolicy`] (feature `drivers`) | §7.7, K-I9, K-I14 |
//!
//! A driver's lifecycle:
//!
//! 1. **Mount.** [`DriverSupervisor::propose`] shows what the person approves — the command
//!    line, the partitions, the effect classes, the sandbox and the secrets it names — and
//!    [`DriverSupervisor::mount`] records the approval (`kernel.driver.mounted`). Nothing runs
//!    before it (K-I13).
//! 2. **Start.** Lazily when a partition's agent lists the driver's tools, or at mount for
//!    `lifecycle = "eager"`. The stdio launcher runs the server under the OS sandbox where an
//!    enforcer exists and records `Unsandboxed { reason }` otherwise, with an environment
//!    cleared to an allowlist and each `secret://` reference minted by the credential broker
//!    into a file only the user can read. The first listing after approval pins each tool's
//!    declaration digest.
//! 3. **Supervise.** The MCP manager's status channel (spec 3.2 §10 T1) becomes
//!    [`KernelEvent::Driver`](crate::KernelEvent::Driver) events and `kernel.driver.*`
//!    records; a crash restarts with the manifest's backoff.
//! 4. **Stale.** A tool whose declaration differs from its pin is stale: the harness's
//!    standing rules pinned to the old declaration stop matching, [`DriverPolicy`] asks
//!    before every call of it, and the driver is `NeedsReapproval` (K-I14) until
//!    [`DriverSupervisor::reapprove`] pins the new declarations and carries the re-approving
//!    person's rules forward.
//! 5. **Park.** A call the server answers with a question ends as a held call: the
//!    [`DriverPark`] is stored beside the approval as the run's checkpoint, the run parks,
//!    and the decision — after any restart — dispatches the call again, which re-issues it
//!    with the answers (spec 3.2 §13 Q9).

use std::fmt;
use std::str::FromStr;

use adk_core::ToolEffect;
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result};

#[cfg(feature = "drivers")]
mod launch;
#[cfg(feature = "drivers")]
mod manifest;
#[cfg(feature = "drivers")]
mod policy;
#[cfg(feature = "drivers")]
pub(crate) mod store;
#[cfg(feature = "drivers")]
mod supervisor;
#[cfg(feature = "drivers")]
mod tool;

#[cfg(feature = "drivers")]
pub use manifest::{
    DriverAccess, DriverLifecycle, DriverManifest, DriverSandbox, SECRET_SCHEME, load_dir,
    load_manifests,
};
#[cfg(feature = "drivers")]
pub use policy::DriverPolicy;
#[cfg(feature = "drivers")]
pub use store::{DriverIsolation, DriverPark, DriverRecord, DriverStore};
#[cfg(feature = "drivers")]
pub(crate) use supervisor::not_mounted;
#[cfg(feature = "drivers")]
pub use supervisor::{
    DriverProposal, DriverSupervisor, DriverSupervisorBuilder, DriverView, StandingRulePins,
};

/// The `ToolOrigin::source` of every tool driver `id` serves: `driver:<id>` (spec 3.2 §7.7).
///
/// A driver's tools report their own source rather than the `mcp:<server>` of an MCP
/// server the harness starts, so a manifest's classes apply only to the tools the kernel
/// supervises.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::drivers::driver_source("email"), "driver:email");
/// assert_eq!(adk_kernel::drivers::driver_of("driver:email"), Some("email"));
/// assert_eq!(adk_kernel::drivers::driver_of("mcp:email"), None);
/// ```
pub fn driver_source(id: &str) -> String {
    format!("{DRIVER_SOURCE_PREFIX}{id}")
}

/// The driver a `ToolOrigin::source` names, if it is a driver's.
pub fn driver_of(source: &str) -> Option<&str> {
    source.strip_prefix(DRIVER_SOURCE_PREFIX).filter(|id| !id.is_empty())
}

/// The prefix of [`driver_source`].
pub const DRIVER_SOURCE_PREFIX: &str = "driver:";

/// What a driver tool does to the world, as its manifest declares it (spec 3.2 §7.7).
///
/// The manifest's class is the source of truth for the harness policy: a server's own
/// `readOnlyHint` never grants anything (spec 3.1 I12). A tool the manifest does not list
/// is [`Other`](Self::Other). [`PublishPublic`](Self::PublishPublic) and
/// [`Financial`](Self::Financial) are never auto-allowed on any path (K-I9).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::EffectClass;
///
/// let class: EffectClass = "send_external".parse().unwrap();
/// assert_eq!(class, EffectClass::SendExternal);
/// assert_eq!(class.to_string(), "send_external");
/// assert!(EffectClass::Financial.is_never_auto_allowed());
/// assert_eq!(EffectClass::Read.tool_effect(), adk_core::ToolEffect::ReadOnly);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EffectClass {
    /// Reads without changing anything.
    Read,
    /// Changes the user's own data on the host or in the service, such as a draft.
    WriteLocal,
    /// Schedules something that involves other people, such as a meeting.
    ScheduleWithOthers,
    /// Sends something to someone outside, such as an email.
    SendExternal,
    /// Publishes something anyone can see.
    PublishPublic,
    /// Moves money.
    Financial,
    /// Deletes something.
    Delete,
    /// Anything else, and every tool the manifest does not list.
    Other,
}

impl EffectClass {
    /// Every class, in declaration order.
    pub const ALL: [EffectClass; 8] = [
        Self::Read,
        Self::WriteLocal,
        Self::ScheduleWithOthers,
        Self::SendExternal,
        Self::PublishPublic,
        Self::Financial,
        Self::Delete,
        Self::Other,
    ];

    /// The class's name as a manifest writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::WriteLocal => "write_local",
            Self::ScheduleWithOthers => "schedule_with_others",
            Self::SendExternal => "send_external",
            Self::PublishPublic => "publish_public",
            Self::Financial => "financial",
            Self::Delete => "delete",
            Self::Other => "other",
        }
    }

    /// The [`ToolEffect`] a driver tool of this class reports: `ReadOnly` for
    /// [`Read`](Self::Read), `NonIdempotent` for every other class, so a call that changes
    /// something is never retried and is recorded in the action ledger.
    pub fn tool_effect(self) -> ToolEffect {
        match self {
            Self::Read => ToolEffect::ReadOnly,
            _ => ToolEffect::NonIdempotent,
        }
    }

    /// Whether no mode, standing rule or run-wide approval may allow a call of this class
    /// without a person deciding it (spec 3.2 K-I9).
    pub fn is_never_auto_allowed(self) -> bool {
        matches!(self, Self::PublishPublic | Self::Financial)
    }

    /// How strictly the harness treats the class: 0 for reads, 1 for changes a standing
    /// rule may allow, 2 for the classes never auto-allowed. A project manifest may only
    /// keep or raise a tool's strictness.
    pub fn strictness(self) -> u8 {
        match self {
            Self::Read => 0,
            Self::PublishPublic | Self::Financial => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for EffectClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EffectClass {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        let normalized = text.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|class| class.as_str() == normalized).ok_or_else(|| {
            KernelError::Config {
                reason: format!(
                    "unknown effect class `{text}`; use one of {}",
                    Self::ALL.map(Self::as_str).join(", ")
                ),
            }
        })
    }
}

/// A driver's state on the event bus (spec 3.2 §7.6): its MCP server's status, or
/// [`NeedsReapproval`](Self::NeedsReapproval) once a tool's declaration differs from the
/// one pinned at approval.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::DriverState;
///
/// assert_eq!(serde_json::to_value(DriverState::NeedsReapproval).unwrap(), "needsReapproval");
/// assert_eq!(DriverState::FailedToStart.as_str(), "failedToStart");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DriverState {
    /// The process is starting and its MCP handshake runs.
    Starting,
    /// The server runs and serves its tools.
    Running,
    /// The server is stopped.
    Stopped,
    /// The process exited or its connection closed unexpectedly.
    Crashed,
    /// The supervisor is restarting the server.
    Restarting,
    /// The server is disabled.
    Disabled,
    /// The server failed to start; after the restart policy's attempts, the exhausted state.
    FailedToStart,
    /// A tool's declaration differs from the pinned one; its standing rules no longer match
    /// and its calls need approval until the driver is re-approved (K-I14).
    NeedsReapproval,
}

#[cfg(feature = "drivers")]
impl From<adk_tool::mcp::ServerStatus> for DriverState {
    fn from(status: adk_tool::mcp::ServerStatus) -> Self {
        use adk_tool::mcp::ServerStatus;
        match status {
            ServerStatus::Starting => Self::Starting,
            ServerStatus::Running => Self::Running,
            ServerStatus::Stopped => Self::Stopped,
            ServerStatus::Crashed => Self::Crashed,
            ServerStatus::Restarting => Self::Restarting,
            ServerStatus::Disabled => Self::Disabled,
            ServerStatus::FailedToStart => Self::FailedToStart,
        }
    }
}

impl DriverState {
    /// The state's name, as it serializes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Crashed => "crashed",
            Self::Restarting => "restarting",
            Self::Disabled => "disabled",
            Self::FailedToStart => "failedToStart",
            Self::NeedsReapproval => "needsReapproval",
        }
    }
}
