use std::collections::BTreeSet;

use adk_guardrail::Severity;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

use super::{PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail};

/// Restricts payment execution to an explicit currency allowlist.
pub struct CurrencyPolicyGuardrail {
    allowed_currencies: BTreeSet<String>,
}

impl CurrencyPolicyGuardrail {
    /// Creates a currency-policy guardrail.
    #[must_use]
    pub fn new<I, S>(allowed_currencies: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { allowed_currencies: allowed_currencies.into_iter().map(Into::into).collect() }
    }
}

impl PaymentPolicyGuardrail for CurrencyPolicyGuardrail {
    fn name(&self) -> &str {
        "currency_policy"
    }

    fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
    ) -> PaymentPolicyDecision {
        let currency = record.cart.total.currency.as_str();

        if self.allowed_currencies.contains(currency) {
            PaymentPolicyDecision::allow()
        } else {
            PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                self.name(),
                format!("currency `{currency}` is not present in the configured allowlist"),
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
            TransactionId::from("tx-currency"),
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
                    unit_price: Money::new("KES", 1_500, 2),
                    total_price: Money::new("KES", 1_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("KES", 1_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("KES", 1_500, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 20, 0).unwrap(),
        )
    }

    #[test]
    fn currency_policy_denies_disallowed_currency() {
        let guardrail = CurrencyPolicyGuardrail::new(["USD", "EUR"]);
        let decision = guardrail.evaluate(&sample_record(), &ProtocolDescriptor::acp("2026-01-30"));

        assert!(decision.is_deny());
        assert_eq!(decision.findings()[0].guardrail, "currency_policy");
    }
}
