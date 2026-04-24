use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AwpVersion, RequesterType, TrustLevel};

/// AWP protocol request envelope.
///
/// Wraps an inbound request with AWP metadata including trust level,
/// requester type, and protocol version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwpRequest {
    pub id: Uuid,
    pub trust_level: TrustLevel,
    pub requester_type: RequesterType,
    pub version: AwpVersion,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CURRENT_VERSION;

    #[test]
    fn test_serde_round_trip() {
        let req = AwpRequest {
            id: Uuid::now_v7(),
            trust_level: TrustLevel::Known,
            requester_type: RequesterType::Agent,
            version: CURRENT_VERSION,
            payload: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: AwpRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, deserialized);
    }

    #[test]
    fn test_camel_case_serialization() {
        let req = AwpRequest {
            id: Uuid::now_v7(),
            trust_level: TrustLevel::Anonymous,
            requester_type: RequesterType::Human,
            version: CURRENT_VERSION,
            payload: serde_json::json!(null),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("trustLevel"));
        assert!(json.contains("requesterType"));
        assert!(!json.contains("trust_level"));
        assert!(!json.contains("requester_type"));
    }
}
