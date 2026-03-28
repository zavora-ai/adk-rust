//! AP2 mandate types aligned to `v0.1-alpha` as of `2026-03-22`.
//!
//! Three mandate kinds model the progressive authorization chain:
//! [`IntentMandate`] → [`CartMandate`] → [`PaymentMandate`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{Money, ProtocolExtensions};

use super::roles::Ap2Role;

// ---------------------------------------------------------------------------
// Mandate status
// ---------------------------------------------------------------------------

/// Lifecycle status shared by all AP2 mandate types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MandateStatus {
    Pending,
    Active,
    Executed,
    Expired,
    Revoked,
}

// ---------------------------------------------------------------------------
// Authority constraints (human-not-present flows)
// ---------------------------------------------------------------------------

/// Constraints required for human-not-present flows per Requirement 6.8.
///
/// An [`IntentMandate`] operating in human-not-present mode **must** carry
/// explicit expiration and authority constraints before a payment can progress
/// to execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityConstraints {
    /// Maximum amount the mandate may authorize.
    pub max_amount: Money,
    /// Merchants allowed under this mandate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_merchants: Vec<String>,
    /// Product classes allowed under this mandate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_product_classes: Vec<String>,
    /// Hard expiration for the authority grant.
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// IntentMandate
// ---------------------------------------------------------------------------

/// Pre-authorization mandate with constraints.
///
/// For human-not-present transactions, `authority_constraints` **must** be
/// present with an explicit expiration (Requirement 6.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentMandate {
    /// Unique mandate identifier.
    pub id: String,
    /// Role that issued this mandate.
    pub issuer_role: Ap2Role,
    /// Role this mandate targets.
    pub target_role: Ap2Role,
    /// Merchant identifier constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant: Option<String>,
    /// Maximum amount constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Money>,
    /// Product class constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_class: Option<String>,
    /// Authority constraints for human-not-present flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_constraints: Option<AuthorityConstraints>,
    /// Current lifecycle status.
    pub status: MandateStatus,
    /// Signature placeholder for future cryptographic verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// When this mandate was created.
    pub created_at: DateTime<Utc>,
    /// When this mandate expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

// ---------------------------------------------------------------------------
// CartMandate
// ---------------------------------------------------------------------------

/// Merchant-signed cart authorization mandate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CartMandate {
    /// Unique mandate identifier.
    pub id: String,
    /// Role that issued this mandate.
    pub issuer_role: Ap2Role,
    /// Role this mandate targets.
    pub target_role: Ap2Role,
    /// Cart identifier being authorized.
    pub cart_id: String,
    /// Total amount for the cart.
    pub amount: Money,
    /// Current lifecycle status.
    pub status: MandateStatus,
    /// Signature placeholder for future cryptographic verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// When this mandate was created.
    pub created_at: DateTime<Utc>,
    /// When this mandate expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

// ---------------------------------------------------------------------------
// PaymentMandate
// ---------------------------------------------------------------------------

/// Payment execution authorization mandate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentMandate {
    /// Unique mandate identifier.
    pub id: String,
    /// Role that issued this mandate.
    pub issuer_role: Ap2Role,
    /// Role this mandate targets.
    pub target_role: Ap2Role,
    /// Reference to the cart mandate being executed.
    pub cart_mandate_id: String,
    /// Payment method identifier.
    pub payment_method: String,
    /// Amount to execute.
    pub amount: Money,
    /// Current lifecycle status.
    pub status: MandateStatus,
    /// Signature placeholder for future cryptographic verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// When this mandate was created.
    pub created_at: DateTime<Utc>,
    /// When this mandate expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
