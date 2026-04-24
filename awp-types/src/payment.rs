//! AWP payment intent types.
//!
//! The AWP `PaymentIntent` is a simpler, owner-policy-driven lifecycle
//! distinct from the ACP/AP2 transaction model in `adk-payments`.
//! It represents the business owner's view of a payment flow.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TrustLevel;

/// State of an AWP payment intent.
///
/// Lifecycle: `Draft → PendingApproval → Approved → Executing → Settled`
///
/// Terminal states: `Settled`, `Rejected`, `Cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentIntentState {
    /// Initial state — intent created but not yet submitted.
    Draft,
    /// Awaiting owner approval (amount exceeds auto-approve threshold).
    PendingApproval,
    /// Owner approved — ready for execution.
    Approved,
    /// Payment is being processed by the payment provider.
    Executing,
    /// Payment completed successfully.
    Settled,
    /// Owner or policy rejected the payment.
    Rejected,
    /// Buyer or system cancelled the payment.
    Cancelled,
}

impl PaymentIntentState {
    /// Whether this is a terminal state (no further transitions allowed).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Rejected | Self::Cancelled)
    }
}

impl std::fmt::Display for PaymentIntentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => write!(f, "draft"),
            Self::PendingApproval => write!(f, "pending_approval"),
            Self::Approved => write!(f, "approved"),
            Self::Executing => write!(f, "executing"),
            Self::Settled => write!(f, "settled"),
            Self::Rejected => write!(f, "rejected"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// An AWP payment intent representing a business owner's view of a payment.
///
/// This is the protocol-level type shared between `adk-awp` and `adk-gateway`.
/// The policy engine in `adk-awp` bridges this to `adk-payments` for execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentIntent {
    /// Unique payment intent identifier.
    pub id: Uuid,
    /// Product or service SKU.
    pub sku: String,
    /// Amount in smallest currency unit (e.g. cents).
    pub amount: u64,
    /// ISO 4217 currency code (e.g. "USD", "KES").
    pub currency: String,
    /// Payment token from the payment provider (set after authorization).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_token: Option<String>,
    /// Current state in the lifecycle.
    pub state: PaymentIntentState,
    /// Trust level of the requester who initiated this intent.
    pub trust_level: TrustLevel,
    /// HMAC-SHA256 signature for integrity verification.
    pub signature: String,
    /// When this intent was created.
    pub created_at: DateTime<Utc>,
    /// When this intent was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Owner-configurable policy for auto-approving or requiring approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPolicy {
    /// Auto-approve if amount is at or below this threshold (in smallest currency unit).
    pub auto_approve_threshold: u64,
    /// Require explicit owner approval above this threshold.
    pub require_approval_threshold: u64,
    /// Minimum trust level for auto-approval.
    pub min_trust_for_auto_approve: TrustLevel,
    /// Notification channel for approval requests (e.g. "email", "sms", "webhook").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notification_channels: Vec<String>,
}

impl Default for PaymentPolicy {
    fn default() -> Self {
        Self {
            auto_approve_threshold: 5000,       // $50.00
            require_approval_threshold: 50_000, // $500.00
            min_trust_for_auto_approve: TrustLevel::Known,
            notification_channels: vec![],
        }
    }
}

impl PaymentPolicy {
    /// Evaluate whether a payment intent should be auto-approved, require
    /// owner approval, or be auto-rejected.
    pub fn evaluate(&self, amount: u64, trust_level: TrustLevel) -> PaymentPolicyDecision {
        if trust_level < self.min_trust_for_auto_approve {
            return PaymentPolicyDecision::RequireApproval;
        }
        if amount <= self.auto_approve_threshold {
            return PaymentPolicyDecision::AutoApprove;
        }
        if amount > self.require_approval_threshold {
            return PaymentPolicyDecision::RequireApproval;
        }
        PaymentPolicyDecision::RequireApproval
    }
}

/// Decision from the payment policy engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentPolicyDecision {
    /// Automatically approve — amount and trust level meet thresholds.
    AutoApprove,
    /// Require explicit owner approval.
    RequireApproval,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payment_intent_state_terminal() {
        assert!(!PaymentIntentState::Draft.is_terminal());
        assert!(!PaymentIntentState::PendingApproval.is_terminal());
        assert!(!PaymentIntentState::Approved.is_terminal());
        assert!(!PaymentIntentState::Executing.is_terminal());
        assert!(PaymentIntentState::Settled.is_terminal());
        assert!(PaymentIntentState::Rejected.is_terminal());
        assert!(PaymentIntentState::Cancelled.is_terminal());
    }

    #[test]
    fn test_payment_intent_state_display() {
        assert_eq!(PaymentIntentState::Draft.to_string(), "draft");
        assert_eq!(PaymentIntentState::PendingApproval.to_string(), "pending_approval");
        assert_eq!(PaymentIntentState::Settled.to_string(), "settled");
    }

    #[test]
    fn test_payment_intent_state_serde() {
        let states = [
            PaymentIntentState::Draft,
            PaymentIntentState::PendingApproval,
            PaymentIntentState::Approved,
            PaymentIntentState::Executing,
            PaymentIntentState::Settled,
            PaymentIntentState::Rejected,
            PaymentIntentState::Cancelled,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: PaymentIntentState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn test_payment_intent_serde_round_trip() {
        let intent = PaymentIntent {
            id: Uuid::now_v7(),
            sku: "WIDGET-001".to_string(),
            amount: 2500,
            currency: "USD".to_string(),
            payment_token: None,
            state: PaymentIntentState::Draft,
            trust_level: TrustLevel::Known,
            signature: "sha256=abc123".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        let parsed: PaymentIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(intent, parsed);
    }

    #[test]
    fn test_payment_intent_optional_token_skipped() {
        let intent = PaymentIntent {
            id: Uuid::now_v7(),
            sku: "SKU".to_string(),
            amount: 100,
            currency: "KES".to_string(),
            payment_token: None,
            state: PaymentIntentState::Draft,
            trust_level: TrustLevel::Anonymous,
            signature: "sig".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        assert!(!json.contains("paymentToken"));
    }

    #[test]
    fn test_payment_policy_default() {
        let policy = PaymentPolicy::default();
        assert_eq!(policy.auto_approve_threshold, 5000);
        assert_eq!(policy.require_approval_threshold, 50_000);
        assert_eq!(policy.min_trust_for_auto_approve, TrustLevel::Known);
    }

    #[test]
    fn test_payment_policy_auto_approve() {
        let policy = PaymentPolicy::default();
        assert_eq!(policy.evaluate(2500, TrustLevel::Known), PaymentPolicyDecision::AutoApprove);
    }

    #[test]
    fn test_payment_policy_require_approval_high_amount() {
        let policy = PaymentPolicy::default();
        assert_eq!(
            policy.evaluate(60_000, TrustLevel::Partner),
            PaymentPolicyDecision::RequireApproval
        );
    }

    #[test]
    fn test_payment_policy_require_approval_low_trust() {
        let policy = PaymentPolicy::default();
        assert_eq!(
            policy.evaluate(1000, TrustLevel::Anonymous),
            PaymentPolicyDecision::RequireApproval
        );
    }

    #[test]
    fn test_payment_policy_serde_round_trip() {
        let policy = PaymentPolicy {
            auto_approve_threshold: 1000,
            require_approval_threshold: 10_000,
            min_trust_for_auto_approve: TrustLevel::Partner,
            notification_channels: vec!["email".to_string(), "sms".to_string()],
        };
        let json = serde_json::to_string(&policy).unwrap();
        let parsed: PaymentPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, parsed);
    }
}
