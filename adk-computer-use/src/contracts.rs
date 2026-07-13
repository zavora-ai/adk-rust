use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

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
    pub resource: Option<ActionResourceContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ActionProvenance>,
    pub data_labels: Vec<String>,
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
