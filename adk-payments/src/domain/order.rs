use serde::{Deserialize, Serialize};

use crate::domain::ProtocolExtensions;
use crate::kernel::PaymentsKernelError;

/// Canonical order lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Draft,
    PendingPayment,
    Authorized,
    FulfillmentPending,
    Fulfilled,
    Completed,
    Canceled,
    Refunded,
    PartiallyRefunded,
    Failed,
}

impl OrderState {
    /// Returns `true` when the transition is allowed by the canonical order
    /// state machine.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        use OrderState::{
            Authorized, Canceled, Completed, Draft, Failed, Fulfilled, FulfillmentPending,
            PartiallyRefunded, PendingPayment, Refunded,
        };

        match (self, next) {
            (from, to) if from == to => true,
            (Draft, PendingPayment | Canceled | Failed) => true,
            (PendingPayment, Authorized | Canceled | Failed) => true,
            (Authorized, FulfillmentPending | Completed | Canceled | Failed) => true,
            (FulfillmentPending, Fulfilled | Completed | Failed) => true,
            (Fulfilled, Completed | PartiallyRefunded | Refunded | Failed) => true,
            (Completed, PartiallyRefunded | Refunded) => true,
            (PartiallyRefunded, PartiallyRefunded | Refunded) => true,
            _ => false,
        }
    }
}

/// Canonical receipt or settlement lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    NotRequested,
    Pending,
    Authorized,
    Settled,
    Failed,
    PartiallyRefunded,
    Refunded,
    Voided,
}

impl ReceiptState {
    /// Returns `true` when the transition is allowed by the canonical receipt
    /// state machine.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        use ReceiptState::{
            Authorized, Failed, NotRequested, PartiallyRefunded, Pending, Refunded, Settled, Voided,
        };

        match (self, next) {
            (from, to) if from == to => true,
            (NotRequested, Pending | Authorized | Failed) => true,
            (Pending, Authorized | Settled | Failed | Voided) => true,
            (Authorized, Settled | Failed | Voided) => true,
            (Settled, PartiallyRefunded | Refunded) => true,
            (PartiallyRefunded, PartiallyRefunded | Refunded) => true,
            _ => false,
        }
    }
}

/// Canonical order and receipt summary attached to a transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    pub state: OrderState,
    pub receipt_state: ReceiptState,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

impl OrderSnapshot {
    /// Applies one canonical order-state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested transition would skip or rewind the
    /// canonical order lifecycle.
    pub fn transition_order_state(
        &mut self,
        next: OrderState,
    ) -> std::result::Result<(), PaymentsKernelError> {
        if !self.state.can_transition_to(next) {
            return Err(PaymentsKernelError::InvalidOrderTransition { from: self.state, to: next });
        }

        self.state = next;
        Ok(())
    }

    /// Applies one canonical receipt-state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested transition would skip or rewind the
    /// canonical receipt lifecycle.
    pub fn transition_receipt_state(
        &mut self,
        next: ReceiptState,
    ) -> std::result::Result<(), PaymentsKernelError> {
        if !self.receipt_state.can_transition_to(next) {
            return Err(PaymentsKernelError::InvalidReceiptTransition {
                from: self.receipt_state,
                to: next,
            });
        }

        self.receipt_state = next;
        Ok(())
    }
}
