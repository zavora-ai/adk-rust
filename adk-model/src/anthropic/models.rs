//! Model discovery API for Anthropic.
//!
//! Provides [`ModelInfo`] and the `list_models` / `get_model` methods on
//! [`AnthropicClient`], wrapping the `GET /v1/models` and
//! `GET /v1/models/{model_id}` endpoints.

use super::client::{AnthropicClient, convert_claudius_error};
use adk_core::AdkError;
use serde::{Deserialize, Serialize};

/// Information about an Anthropic model.
///
/// Contains the model identifier, human-readable display name, and creation
/// timestamp as returned by the Anthropic Models API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
///
/// let client = AnthropicClient::new(AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5-20250929"))?;
/// let models = client.list_models().await?;
/// for model in &models {
///     println!("{}: {} (created {})", model.id, model.display_name, model.created_at);
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Unique model identifier (e.g., "claude-sonnet-4-5-20250929").
    pub id: String,
    /// Human-readable name for the model.
    pub display_name: String,
    /// RFC 3339 datetime string of when the model was created.
    pub created_at: String,
}

impl From<claudius::ModelInfo> for ModelInfo {
    fn from(m: claudius::ModelInfo) -> Self {
        // claudius::ModelInfo.created_at is a time::OffsetDateTime serialized
        // with RFC 3339 via serde. We round-trip through serde_json to get the
        // RFC 3339 string without importing the `time` crate directly.
        let created_at = serde_json::to_value(&m)
            .ok()
            .and_then(|v| v.get("created_at")?.as_str().map(String::from))
            .unwrap_or_default();

        Self { id: m.id, display_name: m.display_name, created_at }
    }
}

impl AnthropicClient {
    /// List available Claude models.
    ///
    /// Calls `GET /v1/models` and returns all model descriptors from the
    /// first page. For paginated access, use [`list_models_paginated`].
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` with structured error context if the
    /// API returns an error.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, AdkError> {
        let response = self.client.list_models(None).await.map_err(convert_claudius_error)?;

        Ok(response.data.into_iter().map(ModelInfo::from).collect())
    }

    /// Get details for a specific model.
    ///
    /// Calls `GET /v1/models/{model_id}` and returns the model descriptor.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` with structured error context if the
    /// API returns an error (e.g., model not found).
    pub async fn get_model(&self, model_id: &str) -> Result<ModelInfo, AdkError> {
        let info = self.client.get_model(model_id).await.map_err(convert_claudius_error)?;

        Ok(ModelInfo::from(info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_info_from_claudius() {
        // Build a claudius::ModelInfo via serde round-trip to avoid
        // importing the `time` crate directly.
        let json = serde_json::json!({
            "id": "claude-sonnet-4-5-20250929",
            "created_at": "2025-09-29T00:00:00Z",
            "display_name": "Claude Sonnet 4.5",
            "type": "model"
        });
        let claudius_model: claudius::ModelInfo = serde_json::from_value(json).unwrap();

        let info = ModelInfo::from(claudius_model);
        assert_eq!(info.id, "claude-sonnet-4-5-20250929");
        assert_eq!(info.display_name, "Claude Sonnet 4.5");
        assert_eq!(info.created_at, "2025-09-29T00:00:00Z");
    }

    #[test]
    fn test_model_info_serialization_roundtrip() {
        let info = ModelInfo {
            id: "claude-3-opus-20240229".to_string(),
            display_name: "Claude 3 Opus".to_string(),
            created_at: "2024-02-29T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        let deserialized: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, deserialized);
    }
}
