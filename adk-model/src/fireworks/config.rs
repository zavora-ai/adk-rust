//! Configuration types for Fireworks AI provider.

use serde::{Deserialize, Serialize};

/// Default Fireworks AI API base URL.
pub const FIREWORKS_API_BASE: &str = "https://api.fireworks.ai/inference/v1";

/// Configuration for Fireworks AI API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::fireworks::FireworksConfig;
///
/// let config = FireworksConfig::new("your-api-key", "accounts/fireworks/models/llama-v3p1-8b-instruct");
///
/// // With a custom base URL
/// let config = FireworksConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FireworksConfig {
    /// Fireworks AI API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for FireworksConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string(),
            base_url: None,
        }
    }
}

impl FireworksConfig {
    /// Create a new Fireworks AI config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
