use adk_guardrail::Severity;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

use super::{PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail};

/// Restricts payment execution to explicit protocol name and version combinations.
pub struct ProtocolVersionGuardrail {
    allowed_protocols: Vec<ProtocolDescriptor>,
}

impl ProtocolVersionGuardrail {
    /// Creates a protocol-version guardrail.
    #[must_use]
    pub fn new<I>(allowed_protocols: I) -> Self
    where
        I: IntoIterator<Item = ProtocolDescriptor>,
    {
        Self { allowed_protocols: allowed_protocols.into_iter().collect() }
    }

    fn allows(&self, protocol: &ProtocolDescriptor) -> bool {
        self.allowed_protocols.iter().any(|allowed| {
            allowed.name == protocol.name
                && match &allowed.version {
                    Some(version) => protocol.version.as_ref() == Some(version),
                    None => true,
                }
        })
    }
}

impl PaymentPolicyGuardrail for ProtocolVersionGuardrail {
    fn name(&self) -> &str {
        "protocol_version"
    }

    fn evaluate(
        &self,
        _record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
    ) -> PaymentPolicyDecision {
        if self.allows(protocol) {
            PaymentPolicyDecision::allow()
        } else {
            let version = protocol.version.as_deref().unwrap_or("unspecified");
            PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                self.name(),
                format!(
                    "protocol `{}` version `{version}` is not allowed by the configured baseline policy",
                    protocol.name
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
            TransactionId::from("tx-protocol"),
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
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 40, 0).unwrap(),
        )
    }

    #[test]
    fn protocol_version_guardrail_denies_unknown_version() {
        let guardrail = ProtocolVersionGuardrail::new([ProtocolDescriptor::acp("2026-01-30")]);
        let decision = guardrail.evaluate(&sample_record(), &ProtocolDescriptor::acp("2025-12-01"));

        assert!(decision.is_deny());
        assert_eq!(decision.findings()[0].guardrail, "protocol_version");
    }
}
