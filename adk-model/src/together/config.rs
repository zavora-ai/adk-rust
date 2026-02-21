//! Configuration types for Together AI provider.

use serde::{Deserialize, Serialize};

/// Default Together AI API base URL.
pub const TOGETHER_API_BASE: &str = "https://api.together.xyz/v1";

/// Configuration for Together AI API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::together::TogetherConfig;
///
/// let config = TogetherConfig::new("your-api-key", "meta-llama/Llama-3.3-70B-Instruct-Turbo");
///
/// // With a custom base URL
/// let config = TogetherConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TogetherConfig {
    /// Together AI API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for TogetherConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "meta-llama/Llama-3.3-70B-Instruct-Turbo".to_string(),
            base_url: None,
        }
    }
}

impl TogetherConfig {
    /// Create a new Together AI config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
