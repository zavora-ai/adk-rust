use adk_guardrail::Severity;

use crate::domain::{
    CommerceMode, InterventionKind, ProtocolDescriptor, TransactionRecord, TransactionState,
};

use super::{PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail};

/// Policy applied when a payment flow may continue autonomously or requires a user return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterventionActionPolicy {
    /// The payment may continue without further user confirmation.
    Allow,
    /// The payment may continue only after the user explicitly confirms.
    RequireUserConfirmation,
    /// The payment must not continue under the current policy.
    Deny,
}

/// Governs when payment interventions may continue autonomously.
pub struct InterventionPolicyGuardrail {
    human_present_policy: InterventionActionPolicy,
    human_not_present_policy: InterventionActionPolicy,
    blocked_kinds: Vec<InterventionKind>,
}

impl InterventionPolicyGuardrail {
    /// Creates a new intervention-policy guardrail.
    #[must_use]
    pub fn new(
        human_present_policy: InterventionActionPolicy,
        human_not_present_policy: InterventionActionPolicy,
    ) -> Self {
        Self { human_present_policy, human_not_present_policy, blocked_kinds: Vec::new() }
    }

    /// Blocks one specific intervention kind outright.
    #[must_use]
    pub fn with_blocked_kind(mut self, kind: InterventionKind) -> Self {
        self.blocked_kinds.push(kind);
        self
    }

    fn mode_policy(&self, mode: CommerceMode) -> InterventionActionPolicy {
        match mode {
            CommerceMode::HumanPresent => self.human_present_policy,
            CommerceMode::HumanNotPresent => self.human_not_present_policy,
        }
    }
}

impl PaymentPolicyGuardrail for InterventionPolicyGuardrail {
    fn name(&self) -> &str {
        "intervention_policy"
    }

    fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
    ) -> PaymentPolicyDecision {
        let policy = self.mode_policy(record.mode);

        if let TransactionState::InterventionRequired(intervention) = &record.state {
            if self.blocked_kinds.contains(&intervention.kind) {
                return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                    self.name(),
                    format!(
                        "intervention `{}` is blocked by policy",
                        intervention_kind_name(&intervention.kind)
                    ),
                    Severity::High,
                )]);
            }

            return match policy {
                InterventionActionPolicy::Allow => PaymentPolicyDecision::allow(),
                InterventionActionPolicy::RequireUserConfirmation => {
                    PaymentPolicyDecision::escalate(vec![PaymentPolicyFinding::new(
                        self.name(),
                        format!(
                            "intervention `{}` requires explicit user confirmation before continuation",
                            intervention_kind_name(&intervention.kind)
                        ),
                        Severity::Medium,
                    )])
                }
                InterventionActionPolicy::Deny => {
                    PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                        self.name(),
                        format!(
                            "intervention `{}` cannot continue under the current policy",
                            intervention_kind_name(&intervention.kind)
                        ),
                        Severity::High,
                    )])
                }
            };
        }

        if matches!(record.mode, CommerceMode::HumanNotPresent) {
            match policy {
                InterventionActionPolicy::Allow => {}
                InterventionActionPolicy::RequireUserConfirmation => {
                    return PaymentPolicyDecision::escalate(vec![PaymentPolicyFinding::new(
                        self.name(),
                        "human-not-present payment execution requires explicit user confirmation"
                            .to_string(),
                        Severity::Medium,
                    )]);
                }
                InterventionActionPolicy::Deny => {
                    return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                        self.name(),
                        "human-not-present payment execution is blocked by policy".to_string(),
                        Severity::High,
                    )]);
                }
            }
        }

        PaymentPolicyDecision::allow()
    }
}

fn intervention_kind_name(kind: &InterventionKind) -> &str {
    match kind {
        InterventionKind::ThreeDsChallenge => "three_ds_challenge",
        InterventionKind::BiometricConfirmation => "biometric_confirmation",
        InterventionKind::AddressVerification => "address_verification",
        InterventionKind::BuyerReconfirmation => "buyer_reconfirmation",
        InterventionKind::MerchantReview => "merchant_review",
        InterventionKind::Other(value) => value.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, InterventionState, InterventionStatus,
        MerchantRef, Money, ProtocolExtensions, TransactionId,
    };

    fn sample_record() -> TransactionRecord {
        let mut record = TransactionRecord::new(
            TransactionId::from("tx-intervention"),
            CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-1".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: None,
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            CommerceMode::HumanNotPresent,
            Cart {
                cart_id: Some("cart-1".to_string()),
                lines: vec![CartLine {
                    line_id: "line-1".to_string(),
                    merchant_sku: Some("sku-1".to_string()),
                    title: "Widget".to_string(),
                    quantity: 1,
                    unit_price: Money::new("USD", 1_500, 2),
                    total_price: Money::new("USD", 1_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", 1_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", 1_500, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 30, 0).unwrap(),
        );
        record
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 15, 31, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::InterventionRequired(Box::new(InterventionState {
                    intervention_id: "int-1".to_string(),
                    kind: InterventionKind::BuyerReconfirmation,
                    status: InterventionStatus::Pending,
                    instructions: Some("return to user".to_string()),
                    continuation_token: None,
                    requested_by: None,
                    expires_at: None,
                    extensions: ProtocolExtensions::default(),
                })),
                Utc.with_ymd_and_hms(2026, 3, 22, 15, 32, 0).unwrap(),
            )
            .unwrap();
        record
    }

    #[test]
    fn intervention_policy_escalates_when_user_confirmation_is_required() {
        let guardrail = InterventionPolicyGuardrail::new(
            InterventionActionPolicy::Allow,
            InterventionActionPolicy::RequireUserConfirmation,
        );
        let decision = guardrail.evaluate(&sample_record(), &ProtocolDescriptor::ap2("v0.1-alpha"));

        assert!(decision.is_escalate());
        assert_eq!(decision.findings()[0].guardrail, "intervention_policy");
    }
}
