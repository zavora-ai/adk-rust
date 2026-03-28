//! AP2 role model aligned to `v0.1-alpha` as of `2026-03-22`.
//!
//! The four explicit AP2 roles map to a subset of [`CommerceActorRole`] from
//! the protocol-neutral domain layer.

use serde::{Deserialize, Serialize};

use crate::domain::{CommerceActorRole, ProtocolExtensions};

/// The four explicit roles defined by the AP2 alpha baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ap2Role {
    Shopper,
    Merchant,
    CredentialsProvider,
    PaymentProcessor,
}

impl Ap2Role {
    /// Converts an AP2 role into the protocol-neutral [`CommerceActorRole`].
    #[must_use]
    pub fn to_commerce_role(self) -> CommerceActorRole {
        match self {
            Self::Shopper => CommerceActorRole::Shopper,
            Self::Merchant => CommerceActorRole::Merchant,
            Self::CredentialsProvider => CommerceActorRole::CredentialsProvider,
            Self::PaymentProcessor => CommerceActorRole::PaymentProcessor,
        }
    }

    /// Attempts to map a [`CommerceActorRole`] back to an AP2 role.
    ///
    /// Returns `None` for roles that have no AP2 equivalent (e.g.
    /// `AgentSurface`, `System`, `Custom`).
    #[must_use]
    pub fn from_commerce_role(role: &CommerceActorRole) -> Option<Self> {
        match role {
            CommerceActorRole::Shopper => Some(Self::Shopper),
            CommerceActorRole::Merchant => Some(Self::Merchant),
            CommerceActorRole::CredentialsProvider => Some(Self::CredentialsProvider),
            CommerceActorRole::PaymentProcessor => Some(Self::PaymentProcessor),
            _ => None,
        }
    }
}

/// Metadata describing an AP2 participant agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2RoleMetadata {
    /// The AP2 role this agent fulfills.
    pub role: Ap2Role,
    /// Unique agent identifier within the AP2 flow.
    pub agent_id: String,
    /// Human-readable display name for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Capabilities advertised by this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Protocol-specific extension fields.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
