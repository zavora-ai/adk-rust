use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AwpVersion;

/// AWP protocol response envelope.
///
/// Wraps an outbound response with AWP metadata including protocol version
/// and status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwpResponse {
    pub id: Uuid,
    pub version: AwpVersion,
    pub status: String,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CURRENT_VERSION;

    #[test]
    fn test_serde_round_trip() {
        let resp = AwpResponse {
            id: Uuid::now_v7(),
            version: CURRENT_VERSION,
            status: "ok".to_string(),
            payload: serde_json::json!({"result": 42}),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: AwpResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, deserialized);
    }
}
