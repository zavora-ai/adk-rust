//! Configuration types for Groq provider.

use serde::{Deserialize, Serialize};

/// Default Groq API base URL.
pub const GROQ_API_BASE: &str = "https://api.groq.com/openai/v1";

/// Configuration for Groq API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroqConfig {
    /// Groq API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Enable reasoning mode (include_reasoning).
    #[serde(default)]
    pub reasoning_enabled: bool,
    /// Maximum tokens for output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl Default for GroqConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "llama-3.3-70b-versatile".to_string(),
            base_url: None,
            reasoning_enabled: false,
            max_tokens: None,
        }
    }
}

impl GroqConfig {
    /// Create a new Groq config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Create a config for llama-3.3-70b-versatile model.
    pub fn llama70b(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "llama-3.3-70b-versatile")
    }

    /// Create a config for llama-3.1-8b-instant model (faster, smaller).
    pub fn llama8b(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "llama-3.1-8b-instant")
    }

    /// Create a config for mixtral-8x7b-32768 model.
    pub fn mixtral(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "mixtral-8x7b-32768")
    }

    /// Create a config for gemma2-9b-it model.
    pub fn gemma(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "gemma2-9b-it")
    }

    /// Enable reasoning mode.
    pub fn with_reasoning(mut self, enabled: bool) -> Self {
        self.reasoning_enabled = enabled;
        self
    }

    /// Set max tokens for output.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Get the effective base URL.
    pub fn effective_base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(GROQ_API_BASE)
    }
}
