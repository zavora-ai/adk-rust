use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// Enforced execution mode selected for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Observe and preview only.
    Shadow,
    /// Proved non-foreground actuation only.
    Background,
    /// Bounded exclusive foreground transaction.
    Foreground,
}

/// Operation-aware action class used by authorization and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Observe,
    Navigate,
    EditReversible,
    CommunicateExternal,
    Authentication,
    Financial,
    Destructive,
    PrivilegeChange,
    SecretAccess,
}

/// Evidence binding an action to a fresh desktop observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetEvidence {
    pub platform: String,
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    pub observation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_tree_revision: Option<String>,
    pub confidence: f64,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActionResourceContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_window_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProvenance {
    pub untrusted_instruction: bool,
    pub source_observation_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crosses_data_boundary: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivityAssessment {
    Sensitive,
    NonSensitive,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySource {
    Accessibility,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySignal {
    SecureRole,
    SecureSubrole,
    ProtectedContent,
    UiaIsPassword,
    SensitiveLabel,
    AmbiguousMatch,
    ElementNotFound,
    InspectionError,
    InvalidField,
    NativeSignalUnavailable,
}

/// Value-free native accessibility evidence used for action risk and revalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTargetSensitivityEvidence", into = "RawTargetSensitivityEvidence")]
pub struct TargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

impl TargetSensitivityEvidence {
    pub fn try_new(
        assessment: TargetSensitivityAssessment,
        source: TargetSensitivitySource,
        signals: Vec<TargetSensitivitySignal>,
        fields_checked: u32,
        observed_at: impl Into<String>,
    ) -> Result<Self, String> {
        RawTargetSensitivityEvidence {
            assessment,
            source,
            signals,
            fields_checked,
            observed_at: observed_at.into(),
        }
        .try_into()
    }

    pub fn assessment(&self) -> TargetSensitivityAssessment {
        self.assessment
    }

    pub fn source(&self) -> TargetSensitivitySource {
        self.source
    }

    pub fn signals(&self) -> &[TargetSensitivitySignal] {
        &self.signals
    }

    pub fn fields_checked(&self) -> u32 {
        self.fields_checked
    }

    pub fn observed_at(&self) -> &str {
        &self.observed_at
    }
}

impl TryFrom<RawTargetSensitivityEvidence> for TargetSensitivityEvidence {
    type Error = String;

    fn try_from(raw: RawTargetSensitivityEvidence) -> Result<Self, Self::Error> {
        if raw.signals.len() > 10 {
            return Err("target sensitivity supports at most 10 signals".into());
        }
        if raw.signals.iter().copied().collect::<HashSet<_>>().len() != raw.signals.len() {
            return Err("target sensitivity signals must be unique".into());
        }
        if raw.fields_checked > 100 {
            return Err("target sensitivity supports at most 100 checked fields".into());
        }
        if matches!(
            raw.assessment,
            TargetSensitivityAssessment::Sensitive | TargetSensitivityAssessment::NonSensitive
        ) && (raw.source != TargetSensitivitySource::Accessibility || raw.fields_checked == 0)
        {
            return Err(
                "conclusive target sensitivity requires accessibility evidence for a checked field"
                    .into(),
            );
        }
        if raw.assessment == TargetSensitivityAssessment::Sensitive && raw.signals.is_empty() {
            return Err("sensitive target evidence requires at least one signal".into());
        }
        if raw.observed_at.trim().is_empty() {
            return Err("target sensitivity observedAt must not be empty".into());
        }
        Ok(Self {
            assessment: raw.assessment,
            source: raw.source,
            signals: raw.signals,
            fields_checked: raw.fields_checked,
            observed_at: raw.observed_at,
        })
    }
}

impl From<TargetSensitivityEvidence> for RawTargetSensitivityEvidence {
    fn from(value: TargetSensitivityEvidence) -> Self {
        Self {
            assessment: value.assessment,
            source: value.source,
            signals: value.signals,
            fields_checked: value.fields_checked,
            observed_at: value.observed_at,
        }
    }
}

fn deserialize_false<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        return Err(de::Error::custom(
            "process postcondition can only prove a non-running process",
        ));
    }
    Ok(false)
}

fn serialize_false<S>(value: &bool, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value {
        return Err(serde::ser::Error::custom(
            "process postcondition can only prove a non-running process",
        ));
    }
    serializer.serialize_bool(false)
}

/// Digest-only expected state independently verified by computer-use-mcp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ActionPostcondition {
    #[serde(rename = "ui_element")]
    UiElement {
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        exists: bool,
        #[serde(rename = "valueDigest", skip_serializing_if = "Option::is_none")]
        value_digest: Option<String>,
    },
    #[serde(rename = "filesystem")]
    Filesystem {
        path: String,
        exists: bool,
        #[serde(rename = "contentDigest", skip_serializing_if = "Option::is_none")]
        content_digest: Option<String>,
    },
    #[serde(rename = "registry")]
    Registry {
        path: String,
        name: String,
        exists: bool,
        #[serde(rename = "valueDigest", skip_serializing_if = "Option::is_none")]
        value_digest: Option<String>,
    },
    #[serde(rename = "process")]
    Process {
        pid: u32,
        #[serde(deserialize_with = "deserialize_false", serialize_with = "serialize_false")]
        running: bool,
    },
    #[serde(rename = "window")]
    Window {
        #[serde(rename = "windowId")]
        window_id: u64,
        exists: bool,
    },
}

/// Immutable action proposed by the graph and enforced by v8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionEnvelope {
    pub action_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    pub principal_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub tool: String,
    pub operation: String,
    pub action_class: ActionClass,
    pub requested_mode: ExecutionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_sensitivity: Option<TargetSensitivityEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResourceContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ActionProvenance>,
    pub data_labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcondition: Option<ActionPostcondition>,
    pub reversible: bool,
    pub external_side_effect: bool,
    pub proposed_at: String,
    pub expires_at: String,
    pub args_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionCapability {
    pub app_id: String,
    pub operation: String,
    pub backend: String,
    pub supported_modes: Vec<ExecutionMode>,
    pub interference: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    pub verification_source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub decision: String,
    pub policy_digest: String,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
}

/// Result of `preview_action` used for deterministic graph routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPreview {
    pub envelope: ActionEnvelope,
    pub executable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    pub policy: PolicyDecision,
    pub capability: ExecutionCapability,
}

/// One-writer desktop control lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLease {
    pub lease_id: String,
    pub revision: u64,
    pub session_id: String,
    pub principal_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub kind: String,
    pub execution_mode: ExecutionMode,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acquired_at: Option<String>,
    pub expires_at: String,
    pub action_budget: u32,
    pub actions_used: u32,
    #[serde(default)]
    pub boundaries: LeaseBoundaries,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBoundaries {
    #[serde(default)]
    pub app_ids: Vec<String>,
    #[serde(default)]
    pub window_ids: Vec<Value>,
    #[serde(default)]
    pub display_ids: Vec<String>,
}

/// Non-authoritative, expiring planner intent for multi-agent conflict checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReservation {
    pub reservation_id: String,
    pub revision: u64,
    pub intent_id: String,
    pub session_id: String,
    pub principal_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub scope: TargetReservationScope,
    pub state: String,
    pub acquired_at: String,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReservationScope {
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
}

/// Terminal status of a v8 execution receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Committed,
    Rejected,
    Interrupted,
    Indeterminate,
}

/// Idempotency receipt returned by `execute_action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionReceipt {
    pub receipt_id: String,
    pub session_id: String,
    pub action_id: String,
    pub action_digest: String,
    pub attempt: u32,
    pub status: ReceiptStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

/// Stable supervisor event used for ADK/v8 trace correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub schema_version: u32,
    pub event_id: String,
    pub sequence: u64,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    pub payload: Value,
}

/// Principal-owned, monotonic steering instruction consumed by an ADK graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFollowUp {
    pub follow_up_id: String,
    pub sequence: u64,
    pub session_id: String,
    pub principal_id: String,
    pub instruction: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFollowUpPage {
    #[serde(rename = "follow_ups")]
    pub follow_ups: Vec<SessionFollowUp>,
    #[serde(rename = "next_sequence")]
    pub next_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSession {
    pub session_id: String,
    pub principal_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    pub state: String,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<SessionCompletionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompletionEvidence {
    pub summary: String,
    pub postconditions: Vec<PostconditionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_window_id: Option<Value>,
    pub action_counts: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostconditionEvidence {
    pub description: String,
    pub satisfied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
}

/// Principal-bound result of deleting one terminal v8 session's durable data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeletionResult {
    pub session_id: String,
    pub deleted: bool,
    pub deleted_events: u64,
    pub deleted_receipts: u64,
    pub deleted_evidence_frames: u64,
    pub revoked_grants: u64,
    pub retained_events: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_marker_id: Option<String>,
}

/// Versioned cross-runtime deterministic safety corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyCorpus {
    pub schema_version: u32,
    pub description: String,
    pub scenarios: Vec<SafetyScenario>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyScenario {
    pub id: String,
    pub fault: String,
    pub expected: SafetyExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyExpectation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_status: Option<String>,
    pub effects: u32,
    pub restores: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_effects: Option<u32>,
}
