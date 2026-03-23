use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{Money, TransactionRecord, TransactionState, TransactionStateTag};
use crate::protocol::ap2::types::{PaymentReceipt, PaymentStatusEnvelope};

/// Safe summary of AP2 mandates for MCP-facing lookup surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2McpMandateStatus {
    pub transaction_id: String,
    pub mode: crate::domain::CommerceMode,
    pub state: TransactionStateTag,
    pub merchant_name: String,
    pub total: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cart_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_required_action: Option<String>,
}

impl Ap2McpMandateStatus {
    /// Builds a safe MCP lookup view from one canonical transaction.
    #[must_use]
    pub fn from_record(record: &TransactionRecord) -> Self {
        Self {
            transaction_id: record.transaction_id.as_str().to_string(),
            mode: record.mode,
            state: record.state.tag(),
            merchant_name: record
                .merchant_of_record
                .display_name
                .clone()
                .unwrap_or_else(|| record.merchant_of_record.legal_name.clone()),
            total: record.cart.total.clone(),
            intent_mandate_id: record.protocol_refs.ap2_intent_mandate_id.clone(),
            cart_mandate_id: record.protocol_refs.ap2_cart_mandate_id.clone(),
            payment_mandate_id: record.protocol_refs.ap2_payment_mandate_id.clone(),
            expires_at: None,
            next_required_action: record.safe_summary.next_required_action.clone(),
        }
    }
}

/// Safe continuation request summary suitable for MCP tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2McpPaymentContinuation {
    pub transaction_id: String,
    pub state: TransactionStateTag,
    pub payment_mandate_id: Option<String>,
    pub continuation_token: Option<String>,
    pub intervention_required: bool,
}

impl Ap2McpPaymentContinuation {
    /// Builds a safe continuation view from one canonical transaction.
    #[must_use]
    pub fn from_record(record: &TransactionRecord) -> Self {
        let (continuation_token, intervention_required) = match &record.state {
            TransactionState::InterventionRequired(intervention) => {
                (intervention.continuation_token.clone(), true)
            }
            TransactionState::Draft
            | TransactionState::Negotiating
            | TransactionState::AwaitingUserAuthorization
            | TransactionState::AwaitingPaymentMethod
            | TransactionState::Authorized
            | TransactionState::Completed
            | TransactionState::Canceled
            | TransactionState::Failed => (None, false),
        };

        Self {
            transaction_id: record.transaction_id.as_str().to_string(),
            state: record.state.tag(),
            payment_mandate_id: record.protocol_refs.ap2_payment_mandate_id.clone(),
            continuation_token,
            intervention_required,
        }
    }
}

/// Safe MCP-facing intervention summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2McpInterventionStatus {
    pub transaction_id: String,
    pub intervention_id: String,
    pub state: TransactionStateTag,
    pub instructions: Option<String>,
    pub continuation_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl Ap2McpInterventionStatus {
    /// Returns a safe intervention view when the transaction currently needs one.
    #[must_use]
    pub fn from_record(record: &TransactionRecord) -> Option<Self> {
        if let TransactionState::InterventionRequired(intervention) = &record.state {
            return Some(Self {
                transaction_id: record.transaction_id.as_str().to_string(),
                intervention_id: intervention.intervention_id.clone(),
                state: record.state.tag(),
                instructions: intervention.instructions.clone(),
                continuation_token: intervention.continuation_token.clone(),
                expires_at: intervention.expires_at,
            });
        }

        None
    }
}

/// Final receipt outcome tag exposed to MCP callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ap2ReceiptStatusKind {
    Success,
    Error,
    Failure,
}

/// Safe MCP-facing payment receipt summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2McpReceiptStatus {
    pub transaction_id: String,
    pub payment_receipt_id: String,
    pub payment_mandate_id: String,
    pub payment_id: String,
    pub amount: Money,
    pub status: Ap2ReceiptStatusKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_confirmation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psp_confirmation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_confirmation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_name: Option<String>,
}

impl Ap2McpReceiptStatus {
    /// Builds a safe receipt view without leaking raw payment credentials.
    #[must_use]
    pub fn from_receipt(record: &TransactionRecord, receipt: &PaymentReceipt) -> Self {
        let (status, merchant_confirmation_id, psp_confirmation_id, network_confirmation_id) =
            match &receipt.payment_status {
                PaymentStatusEnvelope::Success(success) => (
                    Ap2ReceiptStatusKind::Success,
                    Some(success.merchant_confirmation_id.clone()),
                    success.psp_confirmation_id.clone(),
                    success.network_confirmation_id.clone(),
                ),
                PaymentStatusEnvelope::Error(_) => (Ap2ReceiptStatusKind::Error, None, None, None),
                PaymentStatusEnvelope::Failure(_) => {
                    (Ap2ReceiptStatusKind::Failure, None, None, None)
                }
            };

        let method_name = receipt
            .payment_method_details
            .as_ref()
            .and_then(|details| details.get("method_name"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        Self {
            transaction_id: record.transaction_id.as_str().to_string(),
            payment_receipt_id: record
                .protocol_refs
                .ap2_payment_receipt_id
                .clone()
                .unwrap_or_else(|| receipt.payment_id.clone()),
            payment_mandate_id: receipt.payment_mandate_id.clone(),
            payment_id: receipt.payment_id.clone(),
            amount: receipt.amount.to_money(),
            status,
            merchant_confirmation_id,
            psp_confirmation_id,
            network_confirmation_id,
            method_name,
        }
    }
}
