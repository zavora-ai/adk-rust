use serde::{Deserialize, Serialize};

use crate::types::ModelInfo;

/// Response from the list models API endpoint.
///
/// Contains a list of available models and pagination information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelListResponse {
    /// List of models returned by the API.
    pub data: Vec<ModelInfo>,

    /// Indicates whether there are more results available.
    ///
    /// If `true`, there are more models available that can be retrieved
    /// by making another request with pagination parameters.
    pub has_more: bool,

    /// The ID of the first object in the current page.
    ///
    /// Can be used for pagination when requesting the previous page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,

    /// The ID of the last object in the current page.
    ///
    /// Can be used for pagination when requesting the next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
}

impl ModelListResponse {
    /// Create a new `ModelListResponse`.
    pub fn new(
        data: Vec<ModelInfo>,
        has_more: bool,
        first_id: Option<String>,
        last_id: Option<String>,
    ) -> Self {
        Self { data, has_more, first_id, last_id }
    }

    /// Get the list of models.
    pub fn models(&self) -> &[ModelInfo] {
        &self.data
    }

    /// Check if there are more results available.
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Get the first model ID for pagination.
    pub fn first_id(&self) -> Option<&str> {
        self.first_id.as_deref()
    }

    /// Get the last model ID for pagination.
    pub fn last_id(&self) -> Option<&str> {
        self.last_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelType;
    use time::macros::datetime;

    #[test]
    fn model_list_response_serialization() {
        let model_info = ModelInfo {
            id: "claude-sonnet-4-6".to_string(),
            created_at: datetime!(2025-02-19 0:00:00 UTC),
            display_name: "Claude Sonnet 4.6".to_string(),
            r#type: ModelType::Model,
        };

        let response = ModelListResponse::new(
            vec![model_info],
            false,
            Some("first_id".to_string()),
            Some("last_id".to_string()),
        );

        let json = serde_json::to_value(&response).unwrap();
        let expected = serde_json::json!({
            "data": [{
                "id": "claude-sonnet-4-6",
                "created_at": "2025-02-19T00:00:00Z",
                "display_name": "Claude Sonnet 4.6",
                "type": "model"
            }],
            "has_more": false,
            "first_id": "first_id",
            "last_id": "last_id"
        });
        assert_eq!(json, expected);
    }

    #[test]
    fn model_list_response_deserialization() {
        let json = serde_json::json!({
            "data": [{
                "id": "claude-sonnet-4-6",
                "created_at": "2025-02-19T00:00:00Z",
                "display_name": "Claude Sonnet 4.6",
                "type": "model"
            }],
            "has_more": false,
            "first_id": "first_id",
            "last_id": "last_id"
        });
        let response: ModelListResponse = serde_json::from_value(json).unwrap();

        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].id, "claude-sonnet-4-6");
        assert!(!response.has_more);
        assert_eq!(response.first_id, Some("first_id".to_string()));
        assert_eq!(response.last_id, Some("last_id".to_string()));
    }

    #[test]
    fn model_list_response_accessors() {
        let model_info = ModelInfo {
            id: "claude-sonnet-4-6".to_string(),
            created_at: datetime!(2025-02-19 0:00:00 UTC),
            display_name: "Claude Sonnet 4.6".to_string(),
            r#type: ModelType::Model,
        };

        let response =
            ModelListResponse::new(vec![model_info], true, None, Some("last_id".to_string()));

        assert_eq!(response.models().len(), 1);
        assert!(response.has_more());
        assert_eq!(response.first_id(), None);
        assert_eq!(response.last_id(), Some("last_id"));
    }
}
