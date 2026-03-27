use std::fmt;

use adk_auth::{ScopeDenied, check_scopes};
use serde::{Deserialize, Serialize};

use super::PaymentsAuthError;

/// Scope for checkout creation.
pub const PAYMENT_CHECKOUT_CREATE_SCOPE: &str = "payments:checkout:create";

/// Scope for checkout updates.
pub const PAYMENT_CHECKOUT_UPDATE_SCOPE: &str = "payments:checkout:update";

/// Scope for checkout completion.
pub const PAYMENT_CHECKOUT_COMPLETE_SCOPE: &str = "payments:checkout:complete";

/// Scope for checkout cancelation.
pub const PAYMENT_CHECKOUT_CANCEL_SCOPE: &str = "payments:checkout:cancel";

/// Scope for delegated credential use.
pub const PAYMENT_CREDENTIAL_DELEGATE_SCOPE: &str = "payments:credential:delegate";

/// Scope for payment intervention continuation.
pub const PAYMENT_INTERVENTION_CONTINUE_SCOPE: &str = "payments:intervention:continue";

/// Scope for order mutation after checkout.
pub const PAYMENT_ORDER_UPDATE_SCOPE: &str = "payments:order:update";

/// Scope for administrative payment operations.
pub const PAYMENT_ADMIN_SCOPE: &str = "payments:admin";

/// Scope for settlement-specific operations.
pub const PAYMENT_SETTLEMENT_SCOPE: &str = "payments:settlement:write";

/// Scope set for checkout creation.
pub const CHECKOUT_CREATE_SCOPES: &[&str] = &[PAYMENT_CHECKOUT_CREATE_SCOPE];

/// Scope set for checkout updates.
pub const CHECKOUT_UPDATE_SCOPES: &[&str] = &[PAYMENT_CHECKOUT_UPDATE_SCOPE];

/// Scope set for checkout completion.
pub const CHECKOUT_COMPLETE_SCOPES: &[&str] = &[PAYMENT_CHECKOUT_COMPLETE_SCOPE];

/// Scope set for checkout cancelation.
pub const CHECKOUT_CANCEL_SCOPES: &[&str] = &[PAYMENT_CHECKOUT_CANCEL_SCOPE];

/// Scope set for delegated credential usage.
pub const CREDENTIAL_DELEGATE_SCOPES: &[&str] = &[PAYMENT_CREDENTIAL_DELEGATE_SCOPE];

/// Scope set for intervention continuation.
pub const INTERVENTION_CONTINUE_SCOPES: &[&str] = &[PAYMENT_INTERVENTION_CONTINUE_SCOPE];

/// Scope set for order updates.
pub const ORDER_UPDATE_SCOPES: &[&str] = &[PAYMENT_ORDER_UPDATE_SCOPE];

/// Scope set for administrative operations.
pub const ADMIN_SCOPES: &[&str] = &[PAYMENT_ADMIN_SCOPE];

/// Scope set for settlement operations.
pub const SETTLEMENT_SCOPES: &[&str] = &[PAYMENT_SETTLEMENT_SCOPE];

/// Catalog of all currently defined payment scopes.
pub const ALL_PAYMENT_SCOPES: &[&str] = &[
    PAYMENT_CHECKOUT_CREATE_SCOPE,
    PAYMENT_CHECKOUT_UPDATE_SCOPE,
    PAYMENT_CHECKOUT_COMPLETE_SCOPE,
    PAYMENT_CHECKOUT_CANCEL_SCOPE,
    PAYMENT_CREDENTIAL_DELEGATE_SCOPE,
    PAYMENT_INTERVENTION_CONTINUE_SCOPE,
    PAYMENT_ORDER_UPDATE_SCOPE,
    PAYMENT_ADMIN_SCOPE,
    PAYMENT_SETTLEMENT_SCOPE,
];

/// Payment-sensitive operations that map to named scopes and audit resources.
///
/// # Example
///
/// ```
/// use adk_payments::auth::{PaymentOperation, check_payment_operation_scopes};
///
/// let granted = vec!["payments:checkout:create".to_string()];
/// check_payment_operation_scopes(PaymentOperation::CreateCheckout, &granted).unwrap();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentOperation {
    CreateCheckout,
    UpdateCheckout,
    CompleteCheckout,
    CancelCheckout,
    DelegateCredential,
    ContinueIntervention,
    UpdateOrder,
    Settlement,
    Admin,
}

impl PaymentOperation {
    /// Returns the stable operation identifier used in audit metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateCheckout => "checkout_create",
            Self::UpdateCheckout => "checkout_update",
            Self::CompleteCheckout => "checkout_complete",
            Self::CancelCheckout => "checkout_cancel",
            Self::DelegateCredential => "credential_delegate",
            Self::ContinueIntervention => "intervention_continue",
            Self::UpdateOrder => "order_update",
            Self::Settlement => "settlement",
            Self::Admin => "admin",
        }
    }

    /// Returns the audit resource key associated with the operation.
    #[must_use]
    pub const fn audit_resource(self) -> &'static str {
        match self {
            Self::CreateCheckout => "payments.checkout.create",
            Self::UpdateCheckout => "payments.checkout.update",
            Self::CompleteCheckout => "payments.checkout.complete",
            Self::CancelCheckout => "payments.checkout.cancel",
            Self::DelegateCredential => "payments.credential.delegate",
            Self::ContinueIntervention => "payments.intervention.continue",
            Self::UpdateOrder => "payments.order.update",
            Self::Settlement => "payments.settlement.write",
            Self::Admin => "payments.admin",
        }
    }

    /// Returns the scopes required to invoke the operation.
    #[must_use]
    pub const fn required_scopes(self) -> &'static [&'static str] {
        match self {
            Self::CreateCheckout => CHECKOUT_CREATE_SCOPES,
            Self::UpdateCheckout => CHECKOUT_UPDATE_SCOPES,
            Self::CompleteCheckout => CHECKOUT_COMPLETE_SCOPES,
            Self::CancelCheckout => CHECKOUT_CANCEL_SCOPES,
            Self::DelegateCredential => CREDENTIAL_DELEGATE_SCOPES,
            Self::ContinueIntervention => INTERVENTION_CONTINUE_SCOPES,
            Self::UpdateOrder => ORDER_UPDATE_SCOPES,
            Self::Settlement => SETTLEMENT_SCOPES,
            Self::Admin => ADMIN_SCOPES,
        }
    }
}

impl fmt::Display for PaymentOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Checks that the granted scopes authorize one payment operation.
///
/// # Errors
///
/// Returns [`PaymentsAuthError::MissingScopes`] when the granted scopes do not
/// satisfy the operation requirements.
pub fn check_payment_operation_scopes(
    operation: PaymentOperation,
    granted: &[String],
) -> Result<(), PaymentsAuthError> {
    check_scopes(operation.required_scopes(), granted)
        .map_err(|denied| scope_denied(operation, denied))
}

fn scope_denied(operation: PaymentOperation, denied: ScopeDenied) -> PaymentsAuthError {
    PaymentsAuthError::MissingScopes {
        operation,
        required: denied.required,
        missing: denied.missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_check_rejects_missing_scope() {
        let granted = vec![PAYMENT_CHECKOUT_CREATE_SCOPE.to_string()];
        let err = check_payment_operation_scopes(PaymentOperation::CompleteCheckout, &granted)
            .unwrap_err();

        match err {
            PaymentsAuthError::MissingScopes { operation, required, missing } => {
                assert_eq!(operation, PaymentOperation::CompleteCheckout);
                assert_eq!(required, vec![PAYMENT_CHECKOUT_COMPLETE_SCOPE.to_string()]);
                assert_eq!(missing, vec![PAYMENT_CHECKOUT_COMPLETE_SCOPE.to_string()]);
            }
            other => panic!("unexpected auth error: {other}"),
        }
    }
}
