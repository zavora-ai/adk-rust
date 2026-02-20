//! Configuration types for Anthropic provider.

use serde::{Deserialize, Serialize};

/// Configuration for extended thinking mode.
///
/// When enabled, Claude produces `thinking` content blocks with internal
/// reasoning before the final response. The API requires `temperature = 1.0`
/// when thinking is active.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::ThinkingConfig;
///
/// let thinking = ThinkingConfig { budget_tokens: 8192 };
/// assert_eq!(thinking.budget_tokens, 8192);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinkingConfig {
    /// Token budget for thinking (must be > 0).
    pub budget_tokens: u32,
}

/// Configuration for Anthropic API.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::AnthropicConfig;
///
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5-20250929")
///     .with_prompt_caching(true)
///     .with_thinking(8192)
///     .with_beta_feature("prompt-caching-2024-07-31")
///     .with_api_version("2024-01-01");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// Anthropic API key.
    pub api_key: String,
    /// Model name (e.g., "claude-sonnet-4-5-20250929", "claude-3-5-sonnet-20241022").
    pub model: String,
    /// Maximum tokens to generate.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Enable prompt caching with `cache_control` blocks.
    #[serde(default)]
    pub prompt_caching: bool,

    /// Extended thinking configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,

    /// Beta feature headers (e.g., "prompt-caching-2024-07-31").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beta_features: Vec<String>,

    /// Custom API version header override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
}

fn default_max_tokens() -> u32 {
    4096
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            max_tokens: default_max_tokens(),
            base_url: None,
            prompt_caching: false,
            thinking: None,
            beta_features: Vec::new(),
            api_version: None,
        }
    }
}

impl AnthropicConfig {
    /// Create a new Anthropic config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Set the maximum tokens to generate.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set a custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Enable or disable prompt caching.
    ///
    /// When enabled, eligible content blocks will include
    /// `cache_control: {"type": "ephemeral"}` in API requests.
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled;
        self
    }

    /// Enable extended thinking with the given token budget.
    ///
    /// When thinking is enabled, the API request will include
    /// `thinking: {type: "enabled", budget_tokens: N}` and
    /// temperature will be forced to 1.0.
    pub fn with_thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ThinkingConfig { budget_tokens });
        self
    }

    /// Add a beta feature header value.
    ///
    /// Multiple beta features can be added by calling this method
    /// repeatedly. Each value is sent as an `anthropic-beta` header.
    pub fn with_beta_feature(mut self, feature: impl Into<String>) -> Self {
        self.beta_features.push(feature.into());
        self
    }

    /// Set a custom API version header.
    ///
    /// Overrides the default `anthropic-version` header value.
    pub fn with_api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = Some(version.into());
        self
    }
}
