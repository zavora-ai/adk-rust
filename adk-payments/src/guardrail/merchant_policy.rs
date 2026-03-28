use std::collections::BTreeSet;

use adk_guardrail::Severity;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

use super::{PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail};

/// Restricts payment execution to an explicit merchant allowlist.
pub struct MerchantAllowlistGuardrail {
    allowed_merchant_ids: BTreeSet<String>,
}

impl MerchantAllowlistGuardrail {
    /// Creates a merchant allowlist guardrail.
    #[must_use]
    pub fn new<I, S>(allowed_merchant_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { allowed_merchant_ids: allowed_merchant_ids.into_iter().map(Into::into).collect() }
    }
}

impl PaymentPolicyGuardrail for MerchantAllowlistGuardrail {
    fn name(&self) -> &str {
        "merchant_allowlist"
    }

    fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
    ) -> PaymentPolicyDecision {
        if self.allowed_merchant_ids.contains(&record.merchant_of_record.merchant_id) {
            PaymentPolicyDecision::allow()
        } else {
            PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                self.name(),
                format!(
                    "merchant `{}` is not present in the configured allowlist",
                    record.merchant_of_record.merchant_id
                ),
                Severity::High,
            )])
        }
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

    fn sample_record() -> TransactionRecord {
        TransactionRecord::new(
            TransactionId::from("tx-merchant"),
            CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-blocked".to_string(),
                legal_name: "Blocked Merchant LLC".to_string(),
                display_name: Some("Blocked Merchant".to_string()),
                statement_descriptor: None,
                country_code: Some("US".to_string()),
                website: Some("https://blocked.example".to_string()),
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
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 15, 0).unwrap(),
        )
    }

    #[test]
    fn merchant_allowlist_denies_unlisted_merchant() {
        let guardrail = MerchantAllowlistGuardrail::new(["merchant-allowed"]);
        let decision = guardrail.evaluate(&sample_record(), &ProtocolDescriptor::acp("2026-01-30"));

        assert!(decision.is_deny());
        assert_eq!(decision.findings()[0].guardrail, "merchant_allowlist");
    }
}
