//! Configuration types for Perplexity provider.

use serde::{Deserialize, Serialize};

/// Default Perplexity API base URL.
pub const PERPLEXITY_API_BASE: &str = "https://api.perplexity.ai";

/// Configuration for Perplexity API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::perplexity::PerplexityConfig;
///
/// let config = PerplexityConfig::new("your-api-key", "sonar");
///
/// // With a custom base URL
/// let config = PerplexityConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerplexityConfig {
    /// Perplexity API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for PerplexityConfig {
    fn default() -> Self {
        Self { api_key: String::new(), model: "sonar".to_string(), base_url: None }
    }
}

impl PerplexityConfig {
    /// Create a new Perplexity config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
