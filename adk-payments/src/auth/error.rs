use adk_auth::AuthError;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use thiserror::Error;

use super::scopes::PaymentOperation;

/// Local payment-auth errors mapped into the ADK structured error envelope.
#[derive(Debug, Error)]
pub enum PaymentsAuthError {
    #[error(
        "payment operation `{operation}` is missing required scopes {missing:?} (requires {required:?}). Grant the missing scopes before retrying the payment mutation."
    )]
    MissingScopes { operation: PaymentOperation, required: Vec<String>, missing: Vec<String> },

    #[error(
        "authenticated request value `{actual}` conflicts with transaction `{transaction_id}` binding `{binding}` expected `{expected}`. Reuse the original transaction identity instead of rebinding it implicitly."
    )]
    IdentityConflict {
        transaction_id: String,
        binding: &'static str,
        expected: String,
        actual: String,
    },

    #[error(
        "failed to emit a payment audit event: {0}. Restore the configured audit sink before retrying the sensitive payment action."
    )]
    AuditSink(#[from] AuthError),
}

impl From<PaymentsAuthError> for AdkError {
    fn from(value: PaymentsAuthError) -> Self {
        match value {
            PaymentsAuthError::MissingScopes { .. } => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Forbidden,
                "payments.auth.scope_denied",
                value.to_string(),
            ),
            PaymentsAuthError::IdentityConflict { .. } => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Forbidden,
                "payments.auth.identity_conflict",
                value.to_string(),
            ),
            PaymentsAuthError::AuditSink(err) => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "payments.auth.audit_failed",
                err.to_string(),
            )
            .with_source(err),
        }
    }
}
