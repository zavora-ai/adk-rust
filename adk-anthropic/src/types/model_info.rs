use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Information about a specific model.
///
/// This struct contains details about an Anthropic model, including its
/// unique identifier, creation time, display name, and type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Unique model identifier.
    pub id: String,

    /// RFC 3339 datetime string representing the time at which the model was released.
    ///
    /// May be set to an epoch value if the release date is unknown.
    #[serde(rename = "created_at", with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,

    /// A human-readable name for the model.
    #[serde(rename = "display_name")]
    pub display_name: String,

    /// Object type.
    ///
    /// For Models, this is always `"model"`.
    #[serde(rename = "type")]
    pub r#type: ModelType,
}

/// Type of the model object.
///
/// For model objects, this is always "model".
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    /// Model type
    Model,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn model_info_serialization() {
        let model_info = ModelInfo {
            id: "claude-sonnet-4-6".to_string(),
            created_at: datetime!(2025-02-19 0:00:00 UTC),
            display_name: "Claude Sonnet 4.6".to_string(),
            r#type: ModelType::Model,
        };

        let json = serde_json::to_value(&model_info).unwrap();
        let expected = serde_json::json!({
            "id": "claude-sonnet-4-6",
            "created_at": "2025-02-19T00:00:00Z",
            "display_name": "Claude Sonnet 4.6",
            "type": "model"
        });
        assert_eq!(json, expected);
    }

    #[test]
    fn model_info_deserialization() {
        let json = serde_json::json!({
            "id": "claude-sonnet-4-6",
            "created_at": "2025-02-19T00:00:00Z",
            "display_name": "Claude Sonnet 4.6",
            "type": "model"
        });
        let model_info: ModelInfo = serde_json::from_value(json).unwrap();

        assert_eq!(model_info.id, "claude-sonnet-4-6");
        assert_eq!(model_info.created_at, datetime!(2025-02-19 0:00:00 UTC));
        assert_eq!(model_info.display_name, "Claude Sonnet 4.6");
        assert_eq!(model_info.r#type, ModelType::Model);
    }
}
