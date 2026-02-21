//! Configuration types for SambaNova provider.

use serde::{Deserialize, Serialize};

/// Default SambaNova API base URL.
pub const SAMBANOVA_API_BASE: &str = "https://api.sambanova.ai/v1";

/// Configuration for SambaNova API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::sambanova::SambaNovaConfig;
///
/// let config = SambaNovaConfig::new("your-api-key", "Meta-Llama-3.3-70B-Instruct");
///
/// // With a custom base URL
/// let config = SambaNovaConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SambaNovaConfig {
    /// SambaNova API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for SambaNovaConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "Meta-Llama-3.3-70B-Instruct".to_string(),
            base_url: None,
        }
    }
}

impl SambaNovaConfig {
    /// Create a new SambaNova config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
