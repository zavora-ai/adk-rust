//! The changes the agent table makes, their previews, and what removing an agent ends
//! (spec 3.3 §5.7).

use std::fmt;

use adk_core::ApprovalRisk;
use serde::{Deserialize, Serialize};

use crate::agents::record::{AgentDefinition, AgentRecord};

/// A change a person asks the agent table for (spec 3.3 §5.7). Every change is previewed
/// first and applied only with its preview's digest.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentAction, AgentDefinition};
///
/// let add = AgentAction::Create { definition: AgentDefinition::new("Finance", "default") };
/// let pause = AgentAction::Pause { id: "finance".into() };
/// assert_ne!(add.digest(), pause.digest());
/// assert_eq!(serde_json::to_value(&pause).unwrap()["action"], "pause");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum AgentAction {
    /// Add an agent by hand.
    Create {
        /// What it is.
        definition: AgentDefinition,
    },
    /// Change an agent's definition; its area stays.
    Change {
        /// The agent.
        id: String,
        /// The version the change was made against.
        expected_version: u64,
        /// What it becomes.
        definition: AgentDefinition,
    },
    /// Pause an agent: its work stops at its next step.
    Pause {
        /// The agent.
        id: String,
    },
    /// Resume an agent a person paused.
    Resume {
        /// The agent.
        id: String,
    },
    /// Remove an agent, ending its work, routines, waiting decisions and follow-ups.
    Remove {
        /// The agent.
        id: String,
        /// The version the removal was asked against.
        expected_version: u64,
    },
}

impl AgentAction {
    /// The agent it names; `None` for a new one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Change { id, .. }
            | Self::Pause { id }
            | Self::Resume { id }
            | Self::Remove { id, .. } => Some(id),
        }
    }

    /// The action's name: `create`, `change`, `pause`, `resume` or `remove`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Change { .. } => "change",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
            Self::Remove { .. } => "remove",
        }
    }

    /// The digest of the change as asked: the SHA-256, in lowercase hex, of its canonical
    /// JSON with any definition [normalized](AgentDefinition::normalized). A preview carries
    /// it, and the table applies the change only with it.
    pub fn digest(&self) -> String {
        use sha2::Digest as _;

        let canonical = match self {
            Self::Create { definition } => {
                Self::Create { definition: definition.clone().normalized() }
            }
            Self::Change { id, expected_version, definition } => Self::Change {
                id: id.clone(),
                expected_version: *expected_version,
                definition: definition.clone().normalized(),
            },
            other => other.clone(),
        };
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        sha2::Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// What removing an agent ends, named before it happens and recorded when it has (spec 3.3
/// §5.7, S-I16).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::EndedWork;
///
/// let ended = EndedWork::default();
/// assert!(ended.is_empty());
/// assert_eq!(serde_json::to_value(&ended).unwrap()["work"], serde_json::json!([]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EndedWork {
    /// The pids of its processes that are not routines and have not finished.
    #[serde(default)]
    pub work: Vec<String>,
    /// The pids of its routines: its scheduled processes and sleepers.
    #[serde(default)]
    pub routines: Vec<String>,
    /// The ids of the inbox requests waiting for a person that its runs made.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// The ids of its open agenda follow-ups.
    #[serde(default)]
    pub follow_ups: Vec<String>,
    /// How many standing rules pinned to it were revoked; known once it is removed.
    #[serde(default)]
    pub rules: u64,
}

impl EndedWork {
    /// Whether nothing ends.
    pub fn is_empty(&self) -> bool {
        self.work.is_empty()
            && self.routines.is_empty()
            && self.decisions.is_empty()
            && self.follow_ups.is_empty()
            && self.rules == 0
    }
}

/// What a change would do, shown before it is applied (spec 3.3 §5.7): the agent's
/// abilities and risk as it would be, what the change touches, and the digest that confirms
/// it.
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalRisk;
/// use adk_kernel::agents::{AgentAssessment, AgentPreview};
///
/// let preview = AgentPreview::new("finance", "create", "0a1b2c3d", AgentAssessment::new(ApprovalRisk::Medium));
/// assert_eq!(preview.code(), "0a1b2c");
/// assert!(preview.needs_presence() == false);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentPreview {
    /// The agent: the id a new one would get.
    pub id: String,
    /// The action: `create`, `change`, `pause`, `resume` or `remove`.
    pub action: String,
    /// What the agent can do as it would be, in plain sentences.
    #[serde(default)]
    pub abilities: Vec<String>,
    /// The risk of the change: of the agent as it would be when it can do more, low when it
    /// can only do less.
    pub risk: ApprovalRisk,
    /// Whether the agent as it would be can spend money.
    #[serde(default)]
    pub money: bool,
    /// Whether the change lets the agent do more than it could; a new agent always does.
    #[serde(default)]
    pub widens: bool,
    /// The definition fields a change touches, by their wire names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// The definition before a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<AgentDefinition>,
    /// The definition after a create or change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<AgentDefinition>,
    /// What a removal ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<EndedWork>,
    /// The digest that applies the change ([`AgentAction::digest`]).
    pub digest: String,
}

impl AgentPreview {
    /// A preview of `action` on `id` with `digest`, at `assessment`'s risk.
    pub fn new(
        id: impl Into<String>,
        action: impl Into<String>,
        digest: impl Into<String>,
        assessment: crate::agents::AgentAssessment,
    ) -> Self {
        Self {
            id: id.into(),
            action: action.into(),
            abilities: assessment.abilities,
            risk: assessment.risk,
            money: assessment.money,
            widens: false,
            fields: Vec::new(),
            was: None,
            now: None,
            ended: None,
            digest: digest.into(),
        }
    }

    /// The short code a text command confirms the change with: the digest's first six
    /// characters (`/agent remove finance --confirm 3f9a2c`).
    pub fn code(&self) -> &str {
        &self.digest[..self.digest.len().min(6)]
    }

    /// Whether applying it needs the person's presence: a change that lets the agent do
    /// more at high risk or with money (spec 3.3 §7.2).
    pub fn needs_presence(&self) -> bool {
        self.widens && (self.risk >= ApprovalRisk::High || self.money)
    }
}

/// What happened to an agent, as `KernelEvent::Agent` and the `kernel.agent.*` records name
/// it (spec 3.3 §5.7, §10 K7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::AgentChange;
///
/// assert_eq!(AgentChange::CantStart.to_string(), "cantStart");
/// assert_eq!(serde_json::to_value(AgentChange::Created).unwrap(), "created");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AgentChange {
    /// An agent suggested one; it waits for a person.
    Proposed,
    /// A person added it, by hand or by approving a suggestion.
    Created,
    /// A person changed its definition.
    Changed,
    /// A person paused it.
    Paused,
    /// A person resumed it.
    Resumed,
    /// Its removal started.
    Removing,
    /// It was removed.
    Removed,
    /// A suggestion or change was refused.
    Refused,
    /// The host could not build it.
    CantStart,
    /// The kernel built and hosts it.
    Hosted,
}

impl AgentChange {
    /// The change's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Created => "created",
            Self::Changed => "changed",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
            Self::Removing => "removing",
            Self::Removed => "removed",
            Self::Refused => "refused",
            Self::CantStart => "cantStart",
            Self::Hosted => "hosted",
        }
    }
}

impl fmt::Display for AgentChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// An agent as the table lists it (spec 3.3 §5.7): its record, and why the host cannot
/// build it when it cannot ("Can't start").
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentView};
///
/// let record = AgentRecord::new("finance", AgentDefinition::new("Finance", "default"),
///     AgentRecordState::Active, AgentOrigin::ByHand, SubjectRef::human("alice"), chrono::Utc::now());
/// let view = AgentView::new(record).with_cant_start("No key for Anthropic");
/// assert_eq!(serde_json::to_value(&view).unwrap()["cantStart"], "No key for Anthropic");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentView {
    /// The record.
    pub agent: AgentRecord,
    /// Why the host cannot build it, on this kernel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cant_start: Option<String>,
}

impl AgentView {
    /// A view of `agent` that starts.
    pub fn new(agent: AgentRecord) -> Self {
        Self { agent, cant_start: None }
    }

    /// Sets why it cannot start.
    #[must_use]
    pub fn with_cant_start(mut self, reason: impl Into<String>) -> Self {
        self.cant_start = Some(reason.into());
        self
    }
}
