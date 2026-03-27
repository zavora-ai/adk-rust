//! AP2 receipt types aligned to `v0.1-alpha` as of `2026-03-22`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{
    EvidenceReference, MerchantRef, Money, PaymentProcessorRef, ProtocolExtensions,
};

/// Verification status of a payment receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptVerificationStatus {
    Unverified,
    Verified,
    Failed,
}

/// Payment receipt issued after successful payment execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentReceipt {
    /// Unique receipt identifier.
    pub receipt_id: String,
    /// References to the mandates that authorized this payment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mandate_refs: Vec<String>,
    /// Amount paid.
    pub amount: Money,
    /// Merchant that received the payment.
    pub merchant: MerchantRef,
    /// Payment processor that executed the payment.
    pub processor: PaymentProcessorRef,
    /// Verification status of this receipt.
    pub status: ReceiptVerificationStatus,
    /// References to stored evidence artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceReference>,
    /// When this receipt was created.
    pub created_at: DateTime<Utc>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
