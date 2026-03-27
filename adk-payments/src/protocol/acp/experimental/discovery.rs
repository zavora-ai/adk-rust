use serde::{Deserialize, Serialize};

/// ACP discovery document served from `/.well-known/acp.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDiscoveryDocument {
    /// Protocol identification and supported version set.
    pub protocol: AcpDiscoveryProtocol,
    /// Base URL for ACP REST routes.
    pub api_base_url: String,
    /// Transport bindings offered by the merchant deployment.
    pub transports: Vec<AcpDiscoveryTransport>,
    /// Stable seller capabilities that do not vary by session.
    pub capabilities: AcpDiscoveryCapabilities,
}

/// ACP discovery protocol metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDiscoveryProtocol {
    /// Protocol identifier. Always `"acp"`.
    pub name: String,
    /// Latest ACP version served by the merchant.
    pub version: String,
    /// All supported ACP versions in chronological order.
    pub supported_versions: Vec<String>,
    /// Optional operator documentation URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// ACP discovery transport binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpDiscoveryTransport {
    /// REST endpoints rooted at `apiBaseUrl`.
    Rest,
    /// An MCP transport surface backed by the same merchant deployment.
    Mcp,
}

/// Stable ACP service advertised in discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpDiscoveryService {
    /// Checkout-session negotiation and completion.
    Checkout,
    /// Order-status retrieval and synchronization.
    Orders,
    /// Delegated payment-token creation.
    DelegatePayment,
}

/// Stable intervention family advertised in discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDiscoveryInterventionType {
    /// Browser-based 3DS intervention.
    #[serde(rename = "3ds")]
    ThreeDs,
    /// Biometric or device confirmation.
    #[serde(rename = "biometric")]
    Biometric,
    /// Address correction or verification.
    #[serde(rename = "address_verification")]
    AddressVerification,
}

/// High-level extension declaration surfaced in discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDiscoveryExtension {
    /// Extension identifier.
    pub name: String,
    /// Optional extension specification URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Optional JSON Schema URL for the extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// Stable merchant capability set exposed in discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDiscoveryCapabilities {
    /// Services implemented by the merchant deployment.
    pub services: Vec<AcpDiscoveryService>,
    /// Optional extension declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AcpDiscoveryExtension>,
    /// Optional intervention families supported by the merchant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intervention_types: Vec<AcpDiscoveryInterventionType>,
    /// Optional lowercase ISO-4217 currency codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_currencies: Vec<String>,
    /// Optional supported locale tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_locales: Vec<String>,
}
