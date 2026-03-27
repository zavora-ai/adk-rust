use adk_core::Result;
use async_trait::async_trait;

use crate::domain::TransactionRecord;
use crate::kernel::commands::{
    BeginInterventionCommand, CancelCheckoutCommand, CompleteCheckoutCommand,
    ContinueInterventionCommand, CreateCheckoutCommand, DelegatePaymentCommand,
    DelegatedPaymentResult, EvidenceLookup, ExecutePaymentCommand,
    ListUnresolvedTransactionsRequest, OrderUpdateCommand, PaymentExecutionResult,
    StoreEvidenceCommand, StoredEvidence, SyncPaymentOutcomeCommand, TransactionLookup,
    UpdateCheckoutCommand,
};

/// Backend-facing checkout operations shared by ACP and AP2 adapters.
#[async_trait]
pub trait MerchantCheckoutService: Send + Sync {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord>;
    async fn update_checkout(&self, command: UpdateCheckoutCommand) -> Result<TransactionRecord>;
    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>>;
    async fn complete_checkout(
        &self,
        command: CompleteCheckoutCommand,
    ) -> Result<TransactionRecord>;
    async fn cancel_checkout(&self, command: CancelCheckoutCommand) -> Result<TransactionRecord>;
    async fn apply_order_update(&self, command: OrderUpdateCommand) -> Result<TransactionRecord>;
}

/// Backend-facing payment execution operations shared by ACP and AP2 adapters.
#[async_trait]
pub trait PaymentExecutionService: Send + Sync {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> Result<PaymentExecutionResult>;

    async fn sync_payment_outcome(
        &self,
        command: SyncPaymentOutcomeCommand,
    ) -> Result<TransactionRecord>;
}

/// Backend-facing delegated-payment tokenization operations.
#[async_trait]
pub trait DelegatedPaymentService: Send + Sync {
    async fn delegate_payment(
        &self,
        command: DelegatePaymentCommand,
    ) -> Result<DelegatedPaymentResult>;
}

/// Backend-facing intervention lifecycle operations.
#[async_trait]
pub trait InterventionService: Send + Sync {
    async fn begin_intervention(
        &self,
        command: BeginInterventionCommand,
    ) -> Result<TransactionRecord>;

    async fn continue_intervention(
        &self,
        command: ContinueInterventionCommand,
    ) -> Result<TransactionRecord>;
}

/// Durable structured transaction storage keyed by canonical transaction ID.
#[async_trait]
pub trait TransactionStore: Send + Sync {
    async fn upsert(&self, record: TransactionRecord) -> Result<()>;
    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>>;
    async fn list_unresolved(
        &self,
        request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>>;
}

/// Raw evidence storage used to preserve immutable protocol artifacts.
#[async_trait]
pub trait EvidenceStore: Send + Sync {
    async fn store(&self, command: StoreEvidenceCommand) -> Result<StoredEvidence>;
    async fn load(&self, lookup: EvidenceLookup) -> Result<Option<StoredEvidence>>;
}
