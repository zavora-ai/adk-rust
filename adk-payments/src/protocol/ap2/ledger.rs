use std::collections::HashMap;

use adk_core::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::domain::TransactionId;

/// Single-use registry for AP2 payment mandate identifiers.
///
/// `Ap2Adapter` consumes a payment mandate identifier immediately before it
/// calls `PaymentExecutionService::execute_payment`, so one mandate reaches the
/// payment backend at most once, including when it is replayed concurrently or
/// against another transaction. Back this trait with shared durable storage
/// when several adapter instances serve one merchant.
#[async_trait]
pub trait PaymentMandateLedger: Send + Sync {
    /// Atomically records `payment_mandate_id` as consumed by `transaction_id`.
    ///
    /// Returns `Ok(None)` when the identifier was unused and is now consumed,
    /// or `Ok(Some(previous))` with the transaction that consumed it earlier.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store is unavailable; the adapter
    /// then refuses to execute the payment.
    async fn consume(
        &self,
        payment_mandate_id: &str,
        transaction_id: &TransactionId,
    ) -> Result<Option<TransactionId>>;
}

/// Process-local [`PaymentMandateLedger`].
///
/// Consumed identifiers do not survive a restart and are not shared between
/// processes. The adapter additionally records the executed mandate on the
/// durable transaction record, which blocks same-transaction replays after a
/// restart.
///
/// # Example
///
/// ```
/// use adk_payments::domain::TransactionId;
/// use adk_payments::protocol::ap2::{InMemoryPaymentMandateLedger, PaymentMandateLedger};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let ledger = InMemoryPaymentMandateLedger::new();
/// let first = TransactionId::from("tx-1");
/// assert_eq!(ledger.consume("pm-1", &first).await?, None);
/// assert_eq!(ledger.consume("pm-1", &TransactionId::from("tx-2")).await?, Some(first));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryPaymentMandateLedger {
    consumed: RwLock<HashMap<String, TransactionId>>,
}

impl InMemoryPaymentMandateLedger {
    /// Creates an empty in-memory ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PaymentMandateLedger for InMemoryPaymentMandateLedger {
    async fn consume(
        &self,
        payment_mandate_id: &str,
        transaction_id: &TransactionId,
    ) -> Result<Option<TransactionId>> {
        let mut consumed = self.consumed.write().await;
        if let Some(previous) = consumed.get(payment_mandate_id) {
            return Ok(Some(previous.clone()));
        }
        consumed.insert(payment_mandate_id.to_string(), transaction_id.clone());
        Ok(None)
    }
}
