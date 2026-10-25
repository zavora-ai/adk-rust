//! Error codes the wallet refuses and holds payments with (spec 3.5 §7.14, A-I16).

use adk_core::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails};
use awp_types::ErrorCode;
use serde_json::Value;

/// The category has no mandate, so the site may not spend in it.
pub const WALLET_NO_MANDATE_CODE: &str = "wallet.no_mandate";
/// The payee is outside the category mandate's payee rules.
pub const WALLET_PAYEE_OUTSIDE_MANDATE_CODE: &str = "wallet.payee_outside_mandate";
/// The wallet has no adapter for the mandate's rail.
pub const WALLET_RAIL_UNAVAILABLE_CODE: &str = "wallet.rail_unavailable";
/// The payment is in a currency other than the wallet's.
pub const WALLET_CURRENCY_MISMATCH_CODE: &str = "wallet.currency_mismatch";
/// The amount is zero or does not fit the ledger.
pub const WALLET_INVALID_AMOUNT_CODE: &str = "wallet.invalid_amount";
/// The category is paused by an anomaly alarm until an owner resumes it.
pub const WALLET_CATEGORY_PAUSED_CODE: &str = "wallet.category_paused";
/// The idempotency key was already used for a different payment.
pub const WALLET_KEY_REUSED_CODE: &str = "wallet.key_reused";
/// The act needs a person's decision: an owner's, not an agent's or a service's.
pub const WALLET_OWNER_DECISION_REQUIRED_CODE: &str = "wallet.owner_decision_required";
/// The payment is not held, so there is nothing to decide.
pub const WALLET_NOT_HELD_CODE: &str = "wallet.not_held";
/// No payment has this id in the wallet.
pub const WALLET_PAYMENT_NOT_FOUND_CODE: &str = "wallet.payment_not_found";
/// The payment with this key is already executing in another call.
pub const WALLET_PAYMENT_IN_PROGRESS_CODE: &str = "wallet.payment_in_progress";
/// The owner refused the payment asked for with this key.
pub const WALLET_PAYMENT_REJECTED_CODE: &str = "wallet.payment_rejected";
/// The payment asked for with this key failed; nothing was paid.
pub const WALLET_PAYMENT_FAILED_CODE: &str = "wallet.payment_failed";
/// A held payment is decided by approval or rejection only.
pub const WALLET_UNSUPPORTED_DECISION_CODE: &str = "wallet.unsupported_decision";
/// The rail's data does not describe the payment it is attached to.
pub const WALLET_RAIL_DATA_MISMATCH_CODE: &str = "wallet.rail_data_mismatch";
/// The funding provider cannot cover the payment.
pub const WALLET_INSUFFICIENT_FUNDS_CODE: &str = "wallet.insufficient_funds";
/// The policy is malformed.
pub const WALLET_INVALID_POLICY_CODE: &str = "wallet.invalid_policy";
/// A site asked for a wallet that serves another site (A-I22).
pub const WALLET_WRONG_SITE_CODE: &str = "wallet.wrong_site";

/// The details key a wallet refusal carries its AWP error code under (spec 3.5 §7.5), for
/// the edge's error envelope.
pub const AWP_ERROR_DETAIL: &str = "awpError";

/// A payment the wallet refuses by policy (A-I16). The details carry
/// [`ErrorCode::PaymentPolicyRejected`] under [`AWP_ERROR_DETAIL`].
pub(crate) fn refused(code: &'static str, message: impl Into<String>) -> AdkError {
    let mut details = ErrorDetails::default();
    details.metadata.insert(
        AWP_ERROR_DETAIL.to_string(),
        Value::String(ErrorCode::PaymentPolicyRejected.as_str().to_string()),
    );
    AdkError::new(ErrorComponent::Guardrail, ErrorCategory::Forbidden, code, message.into())
        .with_details(details)
}

/// A malformed request or policy.
pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Guardrail, ErrorCategory::InvalidInput, code, message.into())
}

/// An act only an owner may take, attempted by someone else.
pub(crate) fn owner_required(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Guardrail,
        ErrorCategory::Unauthorized,
        WALLET_OWNER_DECISION_REQUIRED_CODE,
        message.into(),
    )
}

/// Something the caller named does not exist.
pub(crate) fn not_found(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Guardrail, ErrorCategory::NotFound, code, message.into())
}

/// A request that conflicts with the wallet's state.
pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Guardrail, ErrorCategory::Conflict, code, message.into())
}
