use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use thiserror::Error;

use crate::domain::{OrderState, ReceiptState};

/// Local kernel errors that map into the ADK structured error envelope.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PaymentsKernelError {
    #[error(
        "invalid transaction state transition from {from:?} to {to:?}. Move through the canonical checkout phases instead of skipping payment state."
    )]
    InvalidTransactionTransition { from: &'static str, to: &'static str },

    #[error(
        "invalid order state transition from {from:?} to {to:?}. Apply authorization and fulfillment updates in canonical order."
    )]
    InvalidOrderTransition { from: OrderState, to: OrderState },

    #[error(
        "invalid receipt state transition from {from:?} to {to:?}. Apply authorization, settlement, and refund updates in canonical order."
    )]
    InvalidReceiptTransition { from: ReceiptState, to: ReceiptState },

    #[error(
        "unsupported canonical action `{action}` for protocol `{protocol}`. Return an explicit unsupported response instead of approximating semantics."
    )]
    UnsupportedAction { action: String, protocol: String },

    #[error(
        "payment policy denied `{action}` for transaction `{transaction_id}`. Require explicit user approval or adjust policy before retrying."
    )]
    PolicyDenied { action: String, transaction_id: String },
}

impl From<PaymentsKernelError> for AdkError {
    fn from(value: PaymentsKernelError) -> Self {
        let message = value.to_string();

        match value {
            PaymentsKernelError::InvalidTransactionTransition { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.transaction.invalid_transition",
                message,
            ),
            PaymentsKernelError::InvalidOrderTransition { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.order.invalid_transition",
                message,
            ),
            PaymentsKernelError::InvalidReceiptTransition { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.receipt.invalid_transition",
                message,
            ),
            PaymentsKernelError::UnsupportedAction { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Unsupported,
                "payments.action.unsupported",
                message,
            ),
            PaymentsKernelError::PolicyDenied { .. } => AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::Forbidden,
                "payments.policy.denied",
                message,
            ),
        }
    }
}
