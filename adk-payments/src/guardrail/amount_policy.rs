use adk_guardrail::Severity;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

use super::{PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail};

/// Enforces soft-review and hard-stop thresholds for transaction totals.
pub struct AmountThresholdGuardrail {
    review_threshold_minor: Option<i64>,
    hard_limit_minor: Option<i64>,
}

impl AmountThresholdGuardrail {
    /// Creates a new amount-threshold guardrail.
    #[must_use]
    pub fn new(review_threshold_minor: Option<i64>, hard_limit_minor: Option<i64>) -> Self {
        Self { review_threshold_minor, hard_limit_minor }
    }
}

impl PaymentPolicyGuardrail for AmountThresholdGuardrail {
    fn name(&self) -> &str {
        "amount_threshold"
    }

    fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
    ) -> PaymentPolicyDecision {
        let amount = record.cart.total.amount_minor;
        let currency = record.cart.total.currency.as_str();

        if let Some(limit) = self.hard_limit_minor
            && amount > limit
        {
            return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                self.name(),
                format!(
                    "transaction total {amount} {currency} exceeds the hard limit of {limit} {currency}"
                ),
                Severity::High,
            )]);
        }

        if let Some(threshold) = self.review_threshold_minor
            && amount > threshold
        {
            return PaymentPolicyDecision::escalate(vec![PaymentPolicyFinding::new(
                self.name(),
                format!(
                    "transaction total {amount} {currency} exceeds the review threshold of {threshold} {currency}"
                ),
                Severity::Medium,
            )]);
        }

        PaymentPolicyDecision::allow()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
        ProtocolExtensions, TransactionId,
    };

    fn sample_record(amount_minor: i64) -> TransactionRecord {
        TransactionRecord::new(
            TransactionId::from("tx-amount"),
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
            CommerceMode::HumanPresent,
            Cart {
                cart_id: Some("cart-1".to_string()),
                lines: vec![CartLine {
                    line_id: "line-1".to_string(),
                    merchant_sku: Some("sku-1".to_string()),
                    title: "Widget".to_string(),
                    quantity: 1,
                    unit_price: Money::new("USD", amount_minor, 2),
                    total_price: Money::new("USD", amount_minor, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", amount_minor, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", amount_minor, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 10, 0).unwrap(),
        )
    }

    #[test]
    fn amount_threshold_escalates_before_hard_limit() {
        let guardrail = AmountThresholdGuardrail::new(Some(5_000), Some(10_000));
        let decision =
            guardrail.evaluate(&sample_record(7_500), &ProtocolDescriptor::acp("2026-01-30"));

        assert!(decision.is_escalate());
        assert_eq!(decision.findings()[0].guardrail, "amount_threshold");
    }
}
