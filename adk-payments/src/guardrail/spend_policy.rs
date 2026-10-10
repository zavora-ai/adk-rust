use adk_core::{SPEND_LIMIT_EXCEEDED_CODE, SpendKey};
use adk_guardrail::Severity;
use async_trait::async_trait;

use crate::domain::{Money, ProtocolDescriptor, TransactionRecord};

use super::{
    PaymentOperation, PaymentPolicyContext, PaymentPolicyDecision, PaymentPolicyFinding,
    PaymentPolicyGuardrail, SpendHold,
};

/// Name under which [`SpendLimitGuardrail`] reports findings.
const NAME: &str = "spend_limit";

/// Converts a USD amount to micro-USD, rounding a finer-than-micro fraction up.
///
/// The spend ledger stores USD, so any other currency is refused rather than converted
/// at a guessed rate.
///
/// # Example
///
/// ```
/// use adk_payments::domain::Money;
/// use adk_payments::guardrail::usd_micro_amount;
///
/// assert_eq!(usd_micro_amount(&Money::new("USD", 1_500, 2)), Ok(15_000_000));
/// assert!(usd_micro_amount(&Money::new("EUR", 1_500, 2)).is_err());
/// ```
///
/// # Errors
///
/// Returns a reason when the amount is not USD, is negative, or does not fit in `u64`.
pub fn usd_micro_amount(amount: &Money) -> Result<u64, String> {
    let text = format!("{} {}", amount.to_decimal_string(), amount.currency);
    if !amount.currency.eq_ignore_ascii_case("USD") {
        return Err(format!("{text} cannot be recorded: the spend ledger records USD only"));
    }
    let minor = u64::try_from(amount.amount_minor)
        .map_err(|_| format!("{text} is negative and cannot be reserved"))?;
    let converted = if amount.scale <= 6 {
        10u64.checked_pow(6 - amount.scale).and_then(|factor| minor.checked_mul(factor))
    } else {
        10u64.checked_pow(amount.scale - 6).map(|divisor| minor.div_ceil(divisor))
    };
    converted.ok_or_else(|| format!("{text} is too large for the spend ledger"))
}

/// Reserves each checkout's total in the [spend ledger](adk_core::SpendLedger) before it
/// completes.
///
/// On [`PaymentOperation::CompleteCheckout`] the guardrail reserves the cart total under
/// the key `org / agent / merchant_id`, where `org` and `agent` come from the
/// [`PaymentPolicyContext`] and fall back to the transaction's session app name and
/// initiating actor. A reservation the ledger refuses denies the payment; an unreachable
/// ledger also denies it. The enforcer commits the hold when the checkout completes and
/// releases it when the checkout fails. Checkout creation moves no money and is allowed.
///
/// # Example
///
/// ```
/// use adk_payments::guardrail::{PaymentPolicySet, SpendLimitGuardrail};
///
/// let policies = PaymentPolicySet::new().with(SpendLimitGuardrail::new());
/// assert!(policies.contains("spend_limit"));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct SpendLimitGuardrail {
    required: bool,
}

impl SpendLimitGuardrail {
    /// Creates a guardrail that denies completion when no spend ledger is configured.
    #[must_use]
    pub const fn new() -> Self {
        Self { required: true }
    }

    /// Creates a guardrail that reserves only when a spend ledger is configured.
    ///
    /// The payment tools add this variant when their policy set has no spend guardrail,
    /// so a ledger on `RunConfig::spend_ledger` caps payments without further setup.
    #[must_use]
    pub const fn when_configured() -> Self {
        Self { required: false }
    }
}

impl Default for SpendLimitGuardrail {
    fn default() -> Self {
        Self::new()
    }
}

fn deny(reason: impl Into<String>, severity: Severity) -> PaymentPolicyDecision {
    PaymentPolicyDecision::deny(vec![PaymentPolicyFinding::new(NAME, reason, severity)])
}

#[async_trait]
impl PaymentPolicyGuardrail for SpendLimitGuardrail {
    fn name(&self) -> &str {
        NAME
    }

    async fn evaluate(
        &self,
        record: &TransactionRecord,
        _protocol: &ProtocolDescriptor,
        context: &PaymentPolicyContext,
    ) -> PaymentPolicyDecision {
        match context.operation() {
            PaymentOperation::CreateCheckout => return PaymentPolicyDecision::allow(),
            PaymentOperation::CompleteCheckout => {}
        }
        let Some(ledger) = context.spend_ledger() else {
            return if self.required {
                deny("no spend ledger is configured for this payment", Severity::High)
            } else {
                PaymentPolicyDecision::allow()
            };
        };
        let amount = match usd_micro_amount(&record.cart.total) {
            Ok(amount) => amount,
            Err(reason) => return deny(reason, Severity::High),
        };
        let org = context.org().map(str::to_string).or_else(|| {
            record.session_identity.as_ref().map(|identity| identity.app_name.to_string())
        });
        let Some(org) = org else {
            return deny("the payment has no organization to attribute spend to", Severity::High);
        };
        let merchant = &record.merchant_of_record.merchant_id;
        if merchant.is_empty() {
            return deny("the payment names no merchant to attribute spend to", Severity::High);
        }
        let agent = context.agent().unwrap_or(&record.initiated_by.actor_id);
        let key = SpendKey::org(org).with_agent(agent).with_vendor(merchant);

        match ledger.reserve(&key, amount).await {
            Ok(reservation) => {
                context.hold(SpendHold {
                    ledger: ledger.clone(),
                    reservation,
                    amount_micro_usd: amount,
                });
                PaymentPolicyDecision::allow()
            }
            Err(error) if error.code == SPEND_LIMIT_EXCEEDED_CODE => {
                deny(error.message, Severity::High)
            }
            Err(error) => deny(
                format!("spend ledger unreachable, payment refused: {}", error.message),
                Severity::Critical,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_amounts_convert_at_every_scale() {
        assert_eq!(usd_micro_amount(&Money::new("usd", 7_500, 2)), Ok(75_000_000));
        assert_eq!(usd_micro_amount(&Money::new("USD", 12, 0)), Ok(12_000_000));
        assert_eq!(usd_micro_amount(&Money::new("USD", 1_000_001, 9)), Ok(1_001));
        assert!(usd_micro_amount(&Money::new("USD", -1, 2)).is_err());
        assert!(usd_micro_amount(&Money::new("USD", i64::MAX, 0)).is_err());
    }
}
