use serde::{Deserialize, Serialize};

use crate::domain::ProtocolExtensions;

/// Canonical actor roles used across commerce flows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceActorRole {
    Shopper,
    Merchant,
    CredentialsProvider,
    PaymentProcessor,
    AgentSurface,
    System,
    Custom(String),
}

/// Generic actor reference attached to canonical commerce actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommerceActor {
    pub actor_id: String,
    pub role: CommerceActorRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Explicit merchant-of-record identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantRef {
    pub merchant_id: String,
    pub legal_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_descriptor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical payment-processor identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentProcessorRef {
    pub processor_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processor_type: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
