use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};

use crate::protocol::ap2::error::Ap2Error;
use crate::protocol::ap2::types::{
    AP2_A2A_EXTENSION_URI, AP2_CART_MANDATE_DATA_KEY, AP2_INTENT_MANDATE_DATA_KEY,
    AP2_PAYMENT_MANDATE_DATA_KEY, Ap2RoleMetadata, CartMandate, IntentMandate, PaymentMandate,
};

/// AP2 AgentCard extension metadata for A2A transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2AgentCardExtension {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub params: Ap2RoleMetadata,
}

impl Ap2AgentCardExtension {
    /// Creates a validated AP2 AgentCard extension object.
    pub fn new(params: Ap2RoleMetadata) -> Result<Self> {
        if params.roles.is_empty() {
            return Err(Ap2Error::MissingA2aRoles.into());
        }

        Ok(Self {
            uri: AP2_A2A_EXTENSION_URI.to_string(),
            description: None,
            required: false,
            params,
        })
    }

    /// Validates one deserialized AP2 AgentCard extension object.
    pub fn validate(&self) -> Result<()> {
        if self.uri != AP2_A2A_EXTENSION_URI {
            return Err(Ap2Error::InvalidExtensionUri {
                uri: self.uri.clone(),
                expected: AP2_A2A_EXTENSION_URI.to_string(),
            }
            .into());
        }
        if self.params.roles.is_empty() {
            return Err(Ap2Error::MissingA2aRoles.into());
        }
        Ok(())
    }
}

/// Minimal A2A part model needed for AP2 data containers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Ap2A2aPart {
    Text {
        text: String,
    },
    Data {
        data: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
    },
}

/// Minimal A2A message profile used for AP2 mandate exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2A2aMessage {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: String,
    pub parts: Vec<Ap2A2aPart>,
}

impl Ap2A2aMessage {
    /// Wraps an `IntentMandate` in an AP2-compatible A2A `Message`.
    #[must_use]
    pub fn intent_mandate(
        message_id: impl Into<String>,
        role: impl Into<String>,
        context_id: Option<String>,
        task_id: Option<String>,
        mandate: &IntentMandate,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            context_id,
            task_id,
            role: role.into(),
            parts: vec![Ap2A2aPart::Data {
                data: Map::from_iter([(AP2_INTENT_MANDATE_DATA_KEY.to_string(), json!(mandate))]),
                metadata: None,
            }],
        }
    }

    /// Wraps a `PaymentMandate` in an AP2-compatible A2A `Message`.
    #[must_use]
    pub fn payment_mandate(
        message_id: impl Into<String>,
        role: impl Into<String>,
        context_id: Option<String>,
        task_id: Option<String>,
        mandate: &PaymentMandate,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            context_id,
            task_id,
            role: role.into(),
            parts: vec![Ap2A2aPart::Data {
                data: Map::from_iter([(AP2_PAYMENT_MANDATE_DATA_KEY.to_string(), json!(mandate))]),
                metadata: None,
            }],
        }
    }

    /// Extracts the first AP2 `IntentMandate` from this message if present.
    pub fn extract_intent_mandate(&self) -> Result<Option<IntentMandate>> {
        extract_from_parts(&self.parts, AP2_INTENT_MANDATE_DATA_KEY)
    }

    /// Extracts the first AP2 `PaymentMandate` from this message if present.
    pub fn extract_payment_mandate(&self) -> Result<Option<PaymentMandate>> {
        extract_from_parts(&self.parts, AP2_PAYMENT_MANDATE_DATA_KEY)
    }
}

/// Minimal A2A artifact profile used for AP2 cart mandate exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2A2aArtifact {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub parts: Vec<Ap2A2aPart>,
}

impl Ap2A2aArtifact {
    /// Wraps a `CartMandate` in an AP2-compatible A2A `Artifact`.
    #[must_use]
    pub fn cart_mandate(
        artifact_id: impl Into<String>,
        name: Option<String>,
        mandate: &CartMandate,
    ) -> Self {
        Self {
            artifact_id: artifact_id.into(),
            name,
            parts: vec![Ap2A2aPart::Data {
                data: Map::from_iter([(AP2_CART_MANDATE_DATA_KEY.to_string(), json!(mandate))]),
                metadata: None,
            }],
        }
    }

    /// Extracts the first AP2 `CartMandate` from this artifact if present.
    pub fn extract_cart_mandate(&self) -> Result<Option<CartMandate>> {
        extract_from_parts(&self.parts, AP2_CART_MANDATE_DATA_KEY)
    }
}

fn extract_from_parts<T>(parts: &[Ap2A2aPart], key: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    parts
        .iter()
        .find_map(|part| match part {
            Ap2A2aPart::Data { data, .. } => data.get(key).cloned(),
            Ap2A2aPart::Text { .. } => None,
        })
        .map_or(Ok(None), |value| {
            serde_json::from_value(value).map(Some).map_err(|err| {
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::InvalidInput,
                    "payments.ap2.a2a.decode_failed",
                    format!("failed to decode AP2 A2A payload for `{key}`: {err}"),
                )
            })
        })
}
