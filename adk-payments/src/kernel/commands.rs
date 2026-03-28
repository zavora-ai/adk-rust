use std::collections::BTreeMap;

use adk_core::AdkIdentity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Cart, CommerceActor, CommerceMode, EvidenceReference, FulfillmentSelection, InterventionState,
    MerchantRef, Money, OrderSnapshot, PaymentMethodSelection, PaymentProcessorRef,
    ProtocolDescriptor, ProtocolExtensions, TransactionId, TransactionRecord,
};

/// Shared metadata supplied with canonical commerce commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommerceContext {
    pub transaction_id: TransactionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
    pub actor: CommerceActor,
    pub merchant_of_record: MerchantRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_processor: Option<PaymentProcessorRef>,
    pub mode: CommerceMode,
    pub protocol: ProtocolDescriptor,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical request to create a checkout session or AP2 cart negotiation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCheckoutCommand {
    pub context: CommerceContext,
    pub cart: Cart,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment: Option<FulfillmentSelection>,
}

/// Canonical request to update checkout state before payment execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckoutCommand {
    pub context: CommerceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cart: Option<Cart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment: Option<FulfillmentSelection>,
}

/// Canonical request to finalize checkout and produce an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteCheckoutCommand {
    pub context: CommerceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_payment_method: Option<PaymentMethodSelection>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical request to cancel a checkout or transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelCheckoutCommand {
    pub context: CommerceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical order-state update emitted after checkout completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderUpdateCommand {
    pub context: CommerceContext,
    pub order: OrderSnapshot,
}

/// Canonical payment-execution request shared by ACP and AP2 adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutePaymentCommand {
    pub context: CommerceContext,
    pub amount: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_payment_method: Option<PaymentMethodSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_evidence_refs: Vec<EvidenceReference>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical payment outcome type used by the execution service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentExecutionOutcome {
    Authorized,
    Completed,
    InterventionRequired,
    Failed,
}

/// Canonical payment execution result returned by payment backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentExecutionResult {
    pub outcome: PaymentExecutionOutcome,
    pub transaction: TransactionRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<OrderSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intervention: Option<InterventionState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_evidence_refs: Vec<EvidenceReference>,
}

/// Canonical delegated-payment allowance preserved across protocol adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatePaymentAllowance {
    pub reason: String,
    pub max_amount: Money,
    pub merchant_id: String,
    pub checkout_session_id: String,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical risk signal attached to delegated-payment requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedRiskSignal {
    pub signal_type: String,
    pub score: i64,
    pub action: String,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical request to tokenize or delegate a payment credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatePaymentCommand {
    pub context: CommerceContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_payment_method: Option<PaymentMethodSelection>,
    pub allowance: DelegatePaymentAllowance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_address: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_signals: Vec<DelegatedRiskSignal>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical delegated-payment result returned by payment-token services.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedPaymentResult {
    pub delegated_payment_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<TransactionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_evidence_refs: Vec<EvidenceReference>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical request to sync asynchronous payment outcomes back into the kernel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPaymentOutcomeCommand {
    pub context: CommerceContext,
    pub outcome: PaymentExecutionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<OrderSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intervention: Option<InterventionState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_evidence_refs: Vec<EvidenceReference>,
}

/// Canonical request to start an intervention flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginInterventionCommand {
    pub context: CommerceContext,
    pub intervention: InterventionState,
}

/// Canonical request to resume or complete an intervention flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueInterventionCommand {
    pub context: CommerceContext,
    pub intervention_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical lookup by transaction identifier and optional session identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionLookup {
    pub transaction_id: TransactionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
}

/// Canonical request to list unresolved transactions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListUnresolvedTransactionsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
}

/// Canonical lookup for one stored evidence artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceLookup {
    pub evidence_ref: EvidenceReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
}

/// Canonical request to persist one evidence artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreEvidenceCommand {
    pub transaction_id: TransactionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
    pub evidence_ref: EvidenceReference,
    pub body: Vec<u8>,
    pub content_type: String,
}

/// Stored evidence returned by the evidence store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvidence {
    pub evidence_ref: EvidenceReference,
    pub body: Vec<u8>,
    pub content_type: String,
}
