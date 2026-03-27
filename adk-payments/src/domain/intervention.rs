use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{CommerceActor, ProtocolExtensions};

/// Canonical intervention classes shared across protocols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionKind {
    ThreeDsChallenge,
    BiometricConfirmation,
    AddressVerification,
    BuyerReconfirmation,
    MerchantReview,
    Other(String),
}

/// Canonical intervention lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionStatus {
    Pending,
    InProgress,
    Satisfied,
    Expired,
    Failed,
    Cancelled,
}

impl InterventionStatus {
    /// Returns `true` when no further continuation is expected.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Satisfied | Self::Expired | Self::Failed | Self::Cancelled)
    }
}

/// Canonical intervention details preserved in transaction state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterventionState {
    pub intervention_id: String,
    pub kind: InterventionKind,
    pub status: InterventionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<CommerceActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
