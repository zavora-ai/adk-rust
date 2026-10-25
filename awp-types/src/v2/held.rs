//! Held calls: capability calls waiting for the owner's decision (spec 3.5 §7.4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::ErrorBody;

/// A capability call the owner must decide (spec 3.5 §7.4).
///
/// The call answers `202 Accepted` with `Location: /awp/v2/calls/{id}`; `GET` on that URL
/// returns this document until the call reaches a terminal state.
///
/// # Example
///
/// ```
/// use awp_types::{HeldCall, HeldCallState};
/// use chrono::Utc;
///
/// let call = HeldCall::pending("c-42", "book", Utc::now());
/// assert_eq!(call.status, "/awp/v2/calls/c-42");
/// assert!(!call.state.is_terminal());
/// assert!(HeldCallState::Completed.is_terminal());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HeldCall {
    /// Call id.
    pub id: String,
    /// Name of the capability called.
    pub capability: String,
    /// Where the call stands.
    pub state: HeldCallState,
    /// Status URL, `/awp/v2/calls/{id}`.
    pub status: String,
    /// When the call was held.
    pub created_at: DateTime<Utc>,
    /// When an undecided call expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The capability's output once the call completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why a rejected or expired call did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl HeldCall {
    /// Creates a pending call with its status URL.
    pub fn pending(
        id: impl Into<String>,
        capability: impl Into<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let id = id.into();
        Self {
            status: Self::status_path(&id),
            id,
            capability: capability.into(),
            state: HeldCallState::Pending,
            created_at,
            expires_at: None,
            result: None,
            error: None,
        }
    }

    /// Returns the status path of call `id`, `/awp/v2/calls/{id}`.
    pub fn status_path(id: &str) -> String {
        format!("/awp/v2/calls/{id}")
    }
}

/// Where a held call stands (spec 3.5 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HeldCallState {
    /// Waiting for the owner.
    Pending,
    /// Approved and executing.
    Approved,
    /// Rejected; the call did not run.
    Rejected,
    /// Not decided in time; the call did not run.
    Expired,
    /// Executed; `result` holds the output.
    Completed,
}

impl HeldCallState {
    /// Returns `true` for states no further transition leaves.
    pub fn is_terminal(self) -> bool {
        match self {
            HeldCallState::Pending | HeldCallState::Approved => false,
            HeldCallState::Rejected | HeldCallState::Expired | HeldCallState::Completed => true,
        }
    }
}
