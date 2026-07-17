//! Session lifecycle, event, follow-up, and deletion contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Stable supervisor event used for ADK/runtime trace correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    /// Schema version of the event envelope.
    pub schema_version: u32,
    /// Unique event identifier.
    pub event_id: String,
    /// Monotonic sequence within the session.
    pub sequence: u64,
    /// The session the event belongs to.
    pub session_id: String,
    /// The action the event relates to, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// The event type (e.g. `action.started`, `action.committed`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// RFC 3339 timestamp of the event.
    pub at: String,
    /// The principal associated with the event, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    /// Event-specific payload.
    pub payload: Value,
}

/// Principal-owned, monotonic steering instruction consumed by an ADK graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFollowUp {
    /// Unique follow-up identifier.
    pub follow_up_id: String,
    /// Monotonic sequence within the session.
    pub sequence: u64,
    /// The session the follow-up belongs to.
    pub session_id: String,
    /// The principal that owns the follow-up.
    pub principal_id: String,
    /// The steering instruction text.
    pub instruction: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

/// A page of follow-ups plus the cursor for the next page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFollowUpPage {
    /// The follow-ups in this page.
    #[serde(rename = "follow_ups")]
    pub follow_ups: Vec<SessionFollowUp>,
    /// The sequence to resume from on the next request.
    #[serde(rename = "next_sequence")]
    pub next_sequence: u64,
}

/// A runtime session and its current lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSession {
    /// Unique session identifier.
    pub session_id: String,
    /// The principal that owns the session.
    pub principal_id: String,
    /// Optional execution group for multi-agent coordination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    /// The session objective, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// Session state (e.g. `active`, `completed`).
    pub state: String,
    /// Monotonic session revision.
    pub revision: u64,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 update timestamp.
    pub updated_at: String,
    /// Reason the session is waiting, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<String>,
    /// Whether the session was recovered after a crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<bool>,
    /// Completion evidence, when the session is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<SessionCompletionEvidence>,
}

/// Evidence describing how a session reached completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompletionEvidence {
    /// Human-readable completion summary.
    pub summary: String,
    /// Postcondition evidence collected at completion.
    pub postconditions: Vec<PostconditionEvidence>,
    /// The last app identifier observed, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_app_id: Option<String>,
    /// The last window identifier observed, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_window_id: Option<Value>,
    /// Per-tool action counts.
    pub action_counts: BTreeMap<String, u64>,
    /// Reason for completion, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// RFC 3339 completion timestamp.
    pub completed_at: String,
}

/// A single postcondition and whether it was satisfied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostconditionEvidence {
    /// Human-readable description of the postcondition.
    pub description: String,
    /// Whether the postcondition was satisfied.
    pub satisfied: bool,
    /// Digest of the evidence backing the check, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
}

/// Principal-bound result of deleting one terminal runtime session's durable data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeletionResult {
    /// The session that was targeted.
    pub session_id: String,
    /// Whether the session was deleted.
    pub deleted: bool,
    /// Number of events deleted.
    pub deleted_events: u64,
    /// Number of receipts deleted.
    pub deleted_receipts: u64,
    /// Number of evidence frames deleted.
    pub deleted_evidence_frames: u64,
    /// Number of approval grants revoked.
    pub revoked_grants: u64,
    /// Number of events retained for compliance.
    pub retained_events: u64,
    /// Retention marker identifier, when events were retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_marker_id: Option<String>,
}
