//! AP2 payment request and response types aligned to `v0.1-alpha`.
//!
//! Structures are loosely aligned with the W3C PaymentRequest model while
//! carrying AP2-specific risk and extension data.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::{Money, ProtocolExtensions};

/// W3C PaymentRequest-aligned structure for AP2 payment initiation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2PaymentRequest {
    /// Unique request identifier.
    pub id: String,
    /// Accepted payment method data (method identifiers and configuration).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub method_data: Vec<Ap2PaymentMethodData>,
    /// Payment details including total and display items.
    pub details: Ap2PaymentDetails,
    /// Optional payment options (e.g. shipping, contact info requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Ap2PaymentOptions>,
    /// Risk assessment data attached to this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_data: Option<Ap2RiskData>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Payment method identifier and optional configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2PaymentMethodData {
    /// Payment method identifier (e.g. `"basic-card"`, `"https://pay.example"`).
    pub supported_methods: String,
    /// Optional method-specific configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Payment details including total and optional display items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2PaymentDetails {
    /// Total amount for the payment.
    pub total: Money,
    /// Optional display line items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_items: Vec<Ap2DisplayItem>,
}

/// A single display item within payment details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2DisplayItem {
    /// Label for the display item.
    pub label: String,
    /// Amount for this item.
    pub amount: Money,
}

/// Optional payment options controlling what information is requested.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2PaymentOptions {
    /// Whether shipping address is requested.
    #[serde(default)]
    pub request_shipping: bool,
    /// Whether payer email is requested.
    #[serde(default)]
    pub request_payer_email: bool,
    /// Whether payer phone is requested.
    #[serde(default)]
    pub request_payer_phone: bool,
    /// Whether payer name is requested.
    #[serde(default)]
    pub request_payer_name: bool,
}

/// AP2 payment response returned after payment method selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2PaymentResponse {
    /// Unique response identifier.
    pub id: String,
    /// The payment method name used.
    pub method_name: String,
    /// Method-specific response details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Payer email if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_email: Option<String>,
    /// Payer phone if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_phone: Option<String>,
    /// Payer name if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_name: Option<String>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Risk assessment container attached to AP2 payment flows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2RiskData {
    /// Risk score (0–100, higher = riskier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
    /// Risk assessment source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Recommended action based on risk assessment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Additional risk signals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<Value>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
