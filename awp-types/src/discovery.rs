use serde::{Deserialize, Serialize};

use crate::{AwpVersion, TrustLevel};

/// AWP discovery document served at `/.well-known/awp.json`.
///
/// Describes a site's AWP capabilities, version, and endpoint URLs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwpDiscoveryDocument {
    pub version: AwpVersion,
    pub site_name: String,
    pub site_description: String,
    pub capability_manifest_url: String,
    pub a2a_endpoint_url: String,
    pub events_endpoint_url: String,
    pub health_endpoint_url: String,
    pub supported_trust_levels: Vec<TrustLevel>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CURRENT_VERSION;

    #[test]
    fn test_serde_round_trip() {
        let doc = AwpDiscoveryDocument {
            version: CURRENT_VERSION,
            site_name: "Test Site".to_string(),
            site_description: "A test site".to_string(),
            capability_manifest_url: "https://example.com/awp/manifest".to_string(),
            a2a_endpoint_url: "https://example.com/awp/a2a".to_string(),
            events_endpoint_url: "https://example.com/awp/events/subscribe".to_string(),
            health_endpoint_url: "https://example.com/awp/health".to_string(),
            supported_trust_levels: vec![
                TrustLevel::Anonymous,
                TrustLevel::Known,
                TrustLevel::Partner,
                TrustLevel::Internal,
            ],
        };
        let json = serde_json::to_string(&doc).unwrap();
        let deserialized: AwpDiscoveryDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, deserialized);
    }
}
