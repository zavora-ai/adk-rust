//! Capability, policy, preview, and execution-receipt contracts.

use super::action::{ActionEnvelope, ExecutionMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A verified execution capability advertised by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionCapability {
    /// Application/bundle identifier, or `*` for any.
    pub app_id: String,
    /// Operation the capability covers.
    pub operation: String,
    /// Backend that provides the capability.
    pub backend: String,
    /// Execution modes the capability supports.
    pub supported_modes: Vec<ExecutionMode>,
    /// Interference classification for the capability.
    pub interference: String,
    /// Confidence in `0.0..=1.0`.
    pub confidence: f64,
    /// RFC 3339 timestamp the capability was last verified, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// Source of the capability verification.
    pub verification_source: String,
}

/// A policy decision for a previewed action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    /// The decision (e.g. `allow`, `confirm`, `deny`).
    pub decision: String,
    /// Digest of the policy that produced the decision.
    pub policy_digest: String,
    /// Human-readable reasons contributing to the decision.
    pub reasons: Vec<String>,
    /// Approval grant identifier, when a grant was issued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
}

/// Result of `preview_action` used for deterministic graph routing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPreview {
    /// The runtime-bound action envelope.
    pub envelope: ActionEnvelope,
    /// Whether the action is currently executable.
    pub executable: bool,
    /// Reason the action is blocked, when not executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    /// The policy decision for the action.
    pub policy: PolicyDecision,
    /// The capability backing the action.
    pub capability: ExecutionCapability,
}

/// Terminal status of an execution receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// The action committed successfully.
    Committed,
    /// The action was rejected before commit.
    Rejected,
    /// The action was interrupted.
    Interrupted,
    /// The commit outcome is indeterminate.
    Indeterminate,
}

/// Idempotency receipt returned by `execute_action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionReceipt {
    /// Unique receipt identifier used for idempotent replay.
    pub receipt_id: String,
    /// The session the receipt belongs to.
    pub session_id: String,
    /// The action the receipt covers.
    pub action_id: String,
    /// Digest of the executed action arguments.
    pub action_digest: String,
    /// Execution attempt number.
    pub attempt: u32,
    /// Terminal status of the execution.
    pub status: ReceiptStatus,
    /// RFC 3339 creation timestamp, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC 3339 update timestamp, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Structured result payload, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Structured error payload, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}
