//! Configuration types for Mistral AI provider.

use serde::{Deserialize, Serialize};

/// Default Mistral AI API base URL.
pub const MISTRAL_API_BASE: &str = "https://api.mistral.ai/v1";

/// Configuration for Mistral AI API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::mistral::MistralConfig;
///
/// let config = MistralConfig::new("your-api-key", "mistral-small-latest");
///
/// // With a custom base URL
/// let config = MistralConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistralConfig {
    /// Mistral AI API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for MistralConfig {
    fn default() -> Self {
        Self { api_key: String::new(), model: "mistral-small-latest".to_string(), base_url: None }
    }
}

impl MistralConfig {
    /// Create a new Mistral AI config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
