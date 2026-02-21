//! Configuration types for Cerebras provider.

use serde::{Deserialize, Serialize};

/// Default Cerebras API base URL.
pub const CEREBRAS_API_BASE: &str = "https://api.cerebras.ai/v1";

/// Configuration for Cerebras API.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::cerebras::CerebrasConfig;
///
/// let config = CerebrasConfig::new("your-api-key", "llama-3.3-70b");
///
/// // With a custom base URL
/// let config = CerebrasConfig::new("your-api-key", "my-model")
///     .with_base_url("https://custom-endpoint.example.com/v1");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CerebrasConfig {
    /// Cerebras API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for CerebrasConfig {
    fn default() -> Self {
        Self { api_key: String::new(), model: "llama-3.3-70b".to_string(), base_url: None }
    }
}

impl CerebrasConfig {
    /// Create a new Cerebras config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
