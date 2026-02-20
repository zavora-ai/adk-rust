//! Configuration types for xAI provider.

use serde::{Deserialize, Serialize};

/// Default xAI API base URL.
pub const XAI_API_BASE: &str = "https://api.x.ai/v1";

/// Configuration for xAI API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XAIConfig {
    /// xAI API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for XAIConfig {
    fn default() -> Self {
        Self { api_key: String::new(), model: "grok-2-latest".to_string(), base_url: None }
    }
}

impl XAIConfig {
    /// Create a new xAI config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}
