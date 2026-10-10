//! Payment-specific guardrails and redaction policies.
//!
//! These types provide commerce-aware policy checks and sensitive-data
//! redaction on top of `adk-guardrail` severity and content-transformation
//! primitives for ACP stable `2026-01-30` and AP2 `v0.1-alpha`
//! (`2026-03-22`).

mod amount_policy;
mod currency_policy;
mod intervention_policy;
mod merchant_policy;
mod policy;
mod protocol_version;
mod redaction;
mod spend_policy;

pub use amount_policy::AmountThresholdGuardrail;
pub use currency_policy::CurrencyPolicyGuardrail;
pub use intervention_policy::{InterventionActionPolicy, InterventionPolicyGuardrail};
pub use merchant_policy::MerchantAllowlistGuardrail;
pub use policy::{
    PaymentOperation, PaymentPolicyContext, PaymentPolicyDecision, PaymentPolicyFinding,
    PaymentPolicyGuardrail, PaymentPolicySet, SpendHold,
};
pub use protocol_version::ProtocolVersionGuardrail;
pub use redaction::{
    SensitivePaymentDataGuardrail, redact_payment_content, redact_payment_text,
    redact_payment_value, redact_telemetry_fields, redact_tool_output,
};
pub use spend_policy::{SpendLimitGuardrail, usd_micro_amount};

/// Evaluates `guardrail` for a checkout creation on a throwaway runtime.
#[cfg(test)]
pub(crate) fn evaluate_now(
    guardrail: &dyn PaymentPolicyGuardrail,
    record: &crate::domain::TransactionRecord,
    protocol: &crate::domain::ProtocolDescriptor,
) -> PaymentPolicyDecision {
    tokio::runtime::Builder::new_current_thread().build().expect("test runtime starts").block_on(
        guardrail.evaluate(
            record,
            protocol,
            &PaymentPolicyContext::new(PaymentOperation::CreateCheckout),
        ),
    )
}
