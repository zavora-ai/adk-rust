//! One-writer control leases and non-authoritative target reservations.

use super::action::ExecutionMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One-writer desktop control lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLease {
    /// Unique lease identifier.
    pub lease_id: String,
    /// Monotonic lease revision.
    pub revision: u64,
    /// The session that holds the lease.
    pub session_id: String,
    /// The principal that holds the lease.
    pub principal_id: String,
    /// Optional agent identifier holding the lease.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Lease kind (e.g. `exclusive`, `cooperative`).
    pub kind: String,
    /// Execution mode the lease authorizes.
    pub execution_mode: ExecutionMode,
    /// Lease state (e.g. `active`).
    pub state: String,
    /// RFC 3339 acquisition timestamp, when active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acquired_at: Option<String>,
    /// RFC 3339 expiry timestamp.
    pub expires_at: String,
    /// Maximum number of actions the lease permits.
    pub action_budget: u32,
    /// Actions used against the lease so far.
    pub actions_used: u32,
    /// Boundaries restricting the lease scope.
    #[serde(default)]
    pub boundaries: LeaseBoundaries,
}

/// App/window/display boundaries restricting a [`ControlLease`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBoundaries {
    /// Application/bundle identifiers within the lease scope.
    #[serde(default)]
    pub app_ids: Vec<String>,
    /// Window identifiers within the lease scope.
    #[serde(default)]
    pub window_ids: Vec<Value>,
    /// Display identifiers within the lease scope.
    #[serde(default)]
    pub display_ids: Vec<String>,
}

/// Non-authoritative, expiring planner intent for multi-agent conflict checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReservation {
    /// Unique reservation identifier.
    pub reservation_id: String,
    /// Monotonic reservation revision.
    pub revision: u64,
    /// The planner intent identifier this reservation serves.
    pub intent_id: String,
    /// The session that holds the reservation.
    pub session_id: String,
    /// The principal that holds the reservation.
    pub principal_id: String,
    /// Optional execution group for multi-agent coordination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    /// Optional agent identifier holding the reservation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The app/window scope of the reservation.
    pub scope: TargetReservationScope,
    /// Reservation state (e.g. `active`).
    pub state: String,
    /// RFC 3339 acquisition timestamp.
    pub acquired_at: String,
    /// RFC 3339 expiry timestamp.
    pub expires_at: String,
    /// Reason the reservation reached a terminal state, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

/// Application/window scope of a [`TargetReservation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReservationScope {
    /// Application/bundle identifier.
    pub app_id: String,
    /// Optional window identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
}
