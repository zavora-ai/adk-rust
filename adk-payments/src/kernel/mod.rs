//! Canonical commerce kernel contracts.
//!
//! The kernel exposes protocol-neutral command types, state-transition errors,
//! cross-protocol correlation, and backend-facing service traits used by ACP
//! stable `2026-01-30` and AP2 `v0.1-alpha` (`2026-03-22`) adapters.

pub mod commands;
pub mod correlator;
pub mod errors;
mod governed;
pub mod service;

pub use commands::{
    BeginInterventionCommand, CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand,
    ContinueInterventionCommand, CreateCheckoutCommand, DelegatePaymentAllowance,
    DelegatePaymentCommand, DelegatedPaymentResult, DelegatedRiskSignal, EvidenceLookup,
    ExecutePaymentCommand, ListUnresolvedTransactionsRequest, OrderUpdateCommand,
    PaymentExecutionOutcome, PaymentExecutionResult, StoreEvidenceCommand, StoredEvidence,
    SyncPaymentOutcomeCommand, TransactionLookup, UpdateCheckoutCommand,
};
pub use correlator::{
    LossyConversionError, ProjectionResult, ProtocolCorrelator, ProtocolOrigin, ProtocolRefKind,
};
pub use errors::PaymentsKernelError;
pub use governed::{
    GovernedCheckoutService, PAYMENT_APPROVAL_DENIED_CODE, PAYMENT_APPROVAL_REQUIRED_CODE,
    PAYMENT_POLICY_DENIED_CODE, PaymentApproval, PaymentApprover, PaymentCaller,
};
pub use service::{
    DelegatedPaymentService, EvidenceStore, InterventionService, MerchantCheckoutService,
    PaymentExecutionService, TransactionStore,
};
