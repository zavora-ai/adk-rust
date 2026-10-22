//! Partition identity and mode (spec 3.2 §7.8).
//!
//! Phase 1 declares only what phase-1 types carry: a process's [`PartitionId`] and the
//! [`PartitionMode`] the harness maps onto its `HarnessMode` (§10 HK1). Every process of
//! phase 1 runs in the [`default`](PartitionId::DEFAULT) partition. The `Partition` record,
//! its store, quotas and crossings are phase 3 and are added here without changing these
//! types.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::KernelError;

/// Names a partition: `work`, `home`, a team (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_kernel::PartitionId;
///
/// let work = PartitionId::new("work");
/// assert_eq!(work.as_str(), "work");
/// assert_eq!(PartitionId::default().as_str(), PartitionId::DEFAULT);
/// assert_eq!(serde_json::to_value(&work).unwrap(), "work");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionId(String);

impl PartitionId {
    /// The partition every process runs in until partitions land (phase 3).
    pub const DEFAULT: &'static str = "default";

    /// A partition id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PartitionId {
    fn default() -> Self {
        Self::new(Self::DEFAULT)
    }
}

impl fmt::Display for PartitionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PartitionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PartitionId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for PartitionId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// A partition's default mode (spec 3.2 §7.8): what its agents may do without asking.
///
/// The `kernel` feature of `adk-harness` maps it one to one onto `HarnessMode` (§10 HK1),
/// and the values serialize as `HarnessMode`'s do (`read_only`, `plan`, `ask`, `auto`).
/// Agentrix's Observe / Suggest / Automate is ReadOnly / Ask / Auto (§4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::PartitionMode;
///
/// let mode: PartitionMode = "read_only".parse().unwrap();
/// assert_eq!(mode, PartitionMode::ReadOnly);
/// assert_eq!(mode.to_string(), "read_only");
/// assert_eq!(serde_json::to_value(PartitionMode::Auto).unwrap(), "auto");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PartitionMode {
    /// Reads and todos only.
    ReadOnly,
    /// Reads and planning; nothing that changes the workspace.
    Plan,
    /// Changes need approval unless a standing rule allows them.
    Ask,
    /// Changes run without approval, within the harness's own limits.
    Auto,
}

impl PartitionMode {
    /// The mode's name: `read_only`, `plan`, `ask` or `auto`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }
}

impl fmt::Display for PartitionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PartitionMode {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "read_only" | "readonly" => Ok(Self::ReadOnly),
            "plan" => Ok(Self::Plan),
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            other => Err(KernelError::Config {
                reason: format!(
                    "unknown partition mode `{other}`; use read_only, plan, ask or auto"
                ),
            }),
        }
    }
}
