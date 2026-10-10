use std::cmp::Ordering;

use adk_guardrail::Severity;

use crate::domain::{Money, ProtocolDescriptor, TransactionRecord};

use async_trait::async_trait;

use super::{
    PaymentPolicyContext, PaymentPolicyDecision, PaymentPolicyFinding, PaymentPolicyGuardrail,
};

/// Enforces soft-review and hard-stop thresholds for transaction totals.
///
/// Totals and thresholds are compared by value, so a total expressed at a
/// finer scale (for example `40.000 USD` at scale 3) is compared correctly with
/// a threshold expressed in cents.
///
/// # Example
///
/// ```
/// use adk_payments::guardrail::AmountThresholdGuardrail;
///
/// // Review above 50.00 USD and deny above 100.00 USD; other currencies are denied.
/// let guardrail = AmountThresholdGuardrail::new(Some(5_000), Some(10_000)).with_currency("USD", 2);
/// # let _ = guardrail;
/// ```
pub struct AmountThresholdGuardrail {
    review_threshold_minor: Option<i64>,
    hard_limit_minor: Option<i64>,
    currency: Option<(String, u32)>,
}

impl AmountThresholdGuardrail {
    /// Creates a new amount-threshold guardrail.
    ///
    /// Without [`with_currency`](Self::with_currency), thresholds are minor
    /// units of whichever currency the transaction uses, at that currency's
    /// ISO 4217 minor-unit scale (`5_000` is 50.00 USD but 5,000 JPY). Bind the
    /// thresholds to one currency when transactions can use several.
    #[must_use]
    pub fn new(review_threshold_minor: Option<i64>, hard_limit_minor: Option<i64>) -> Self {
        Self { review_threshold_minor, hard_limit_minor, currency: None }
    }

    /// Expresses the thresholds in `currency` minor units at `scale`.
    ///
    /// Transactions in any other currency are denied, because comparing
    /// amounts across currencies is meaningless without an exchange rate.
    #[must_use]
    pub fn with_currency(mut self, currency: impl Into<String>, scale: u32) -> Self {
        self.currency = Some((currency.into(), scale));
        self
    }
}

#[async_trait]
impl PaymentPolicyGuardrail for AmountThresholdGuardrail {
    fn name(&self) -> &str {
        "amount_threshold"
    }

    async fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
        _context: &PaymentPolicyContext,
    ) -> PaymentPolicyDecision {
        let total = &record.cart.total;
        let total_text = format!("{} {}", total.to_decimal_string(), total.currency);
        let (currency, scale) = match &self.currency {
            Some((currency, scale)) if currency.eq_ignore_ascii_case(&total.currency) => {
                (currency.clone(), *scale)
            }
            Some((currency, _)) => {
                return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                    self.name(),
                    format!(
                        "transaction total {total_text} is not in the guardrail currency {currency}"
                    ),
                    Severity::High,
                )]);
            }
            None => (
                total.currency.clone(),
                Money::iso_minor_unit_scale(&total.currency).unwrap_or(total.scale),
            ),
        };

        // `Ok(Some(reason))` when the total exceeds the limit; `Err` when the
        // two amounts cannot be aligned, which fails closed.
        let exceeds = |limit_minor: i64, label: &str| -> Result<Option<String>, String> {
            let limit = Money::new(currency.clone(), limit_minor, scale);
            let limit_text = format!("{} {currency}", limit.to_decimal_string());
            match total.compare_amount(&limit) {
                Some(Ordering::Greater) => Ok(Some(format!(
                    "transaction total {total_text} exceeds the {label} of {limit_text}"
                ))),
                Some(Ordering::Less | Ordering::Equal) => Ok(None),
                None => Err(format!(
                    "transaction total {total_text} cannot be compared with the {label} of {limit_text}"
                )),
            }
        };

        if let Some(limit) = self.hard_limit_minor {
            match exceeds(limit, "hard limit") {
                Ok(Some(reason)) | Err(reason) => {
                    return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                        self.name(),
                        reason,
                        Severity::High,
                    )]);
                }
                Ok(None) => {}
            }
        }

        if let Some(threshold) = self.review_threshold_minor {
            match exceeds(threshold, "review threshold") {
                Ok(Some(reason)) => {
                    return PaymentPolicyDecision::escalate(vec![PaymentPolicyFinding::new(
                        self.name(),
                        reason,
                        Severity::Medium,
                    )]);
                }
                Err(reason) => {
                    return PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(
                        self.name(),
                        reason,
                        Severity::High,
                    )]);
                }
                Ok(None) => {}
            }
        }

        PaymentPolicyDecision::allow()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef,
        ProtocolExtensions, TransactionId,
    };

    fn sample_record(total: Money) -> TransactionRecord {
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
                    unit_price: total.clone(),
                    total_price: total.clone(),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(total.clone()),
                adjustments: Vec::new(),
                total,
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 15, 10, 0).unwrap(),
        )
    }

    fn evaluate(guardrail: &AmountThresholdGuardrail, total: Money) -> PaymentPolicyDecision {
        crate::guardrail::evaluate_now(
            guardrail,
            &sample_record(total),
            &ProtocolDescriptor::acp("2026-01-30"),
        )
    }

    fn finding(reason: &str, severity: Severity) -> Vec<PaymentPolicyFinding> {
        vec![PaymentPolicyFinding::new("amount_threshold", reason, severity)]
    }

    #[test]
    fn amount_threshold_escalates_before_hard_limit() {
        let guardrail = AmountThresholdGuardrail::new(Some(5_000), Some(10_000));

        assert_eq!(
            evaluate(&guardrail, Money::new("USD", 7_500, 2)),
            PaymentPolicyDecision::escalate(finding(
                "transaction total 75.00 USD exceeds the review threshold of 50.00 USD",
                Severity::Medium,
            ))
        );
    }

    #[test]
    fn three_decimal_total_is_compared_by_value() {
        let guardrail = AmountThresholdGuardrail::new(Some(5_000), Some(10_000));

        assert_eq!(
            evaluate(&guardrail, Money::new("USD", 40_000, 3)),
            PaymentPolicyDecision::allow()
        );
        assert_eq!(
            evaluate(&guardrail, Money::new("USD", 100_001, 3)),
            PaymentPolicyDecision::deny(finding(
                "transaction total 100.001 USD exceeds the hard limit of 100.00 USD",
                Severity::High,
            ))
        );
    }

    #[test]
    fn currency_agnostic_thresholds_use_the_currency_minor_unit() {
        let guardrail = AmountThresholdGuardrail::new(None, Some(5_000));

        assert_eq!(
            evaluate(&guardrail, Money::new("JPY", 4_000, 0)),
            PaymentPolicyDecision::allow()
        );
        assert_eq!(
            evaluate(&guardrail, Money::new("JPY", 10_000, 0)),
            PaymentPolicyDecision::deny(finding(
                "transaction total 10000 JPY exceeds the hard limit of 5000 JPY",
                Severity::High,
            ))
        );
    }

    #[test]
    fn currency_bound_thresholds_deny_other_currencies() {
        let guardrail =
            AmountThresholdGuardrail::new(Some(5_000), Some(10_000)).with_currency("USD", 2);

        assert_eq!(
            evaluate(&guardrail, Money::new("JPY", 100, 0)),
            PaymentPolicyDecision::deny(finding(
                "transaction total 100 JPY is not in the guardrail currency USD",
                Severity::High,
            ))
        );
        assert_eq!(
            evaluate(&guardrail, Money::new("usd", 4_999, 2)),
            PaymentPolicyDecision::allow()
        );
    }

    #[test]
    fn unalignable_amounts_fail_closed() {
        let guardrail = AmountThresholdGuardrail::new(Some(5_000), None).with_currency("USD", 2);

        assert!(evaluate(&guardrail, Money::new("USD", 1, 40)).is_deny());
    }
}
