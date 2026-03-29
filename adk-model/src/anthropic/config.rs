//! Configuration types for Anthropic provider.

use adk_anthropic::ToolSearchConfig;
use serde::{Deserialize, Serialize};

/// Thinking mode configuration for Anthropic models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingMode {
    /// Budget-based thinking (legacy, deprecated on 4.6 models).
    /// Requires `budget_tokens` < `max_tokens`.
    Enabled {
        /// Token budget for thinking (must be ≥ 1024).
        budget_tokens: u32,
    },
    /// Adaptive thinking for Opus 4.6 / Sonnet 4.6.
    /// Claude decides when and how much to think.
    /// Control depth via `effort` on `AnthropicConfig`.
    Adaptive,
}

/// Effort level controlling response thoroughness.
/// Passed via `output_config.effort` in the API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    /// Opus 4.6 only.
    Max,
}

/// Configuration for Anthropic API.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::{AnthropicConfig, ThinkingMode, Effort};
///
/// // Adaptive thinking with medium effort (recommended for Sonnet 4.6)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
///     .with_thinking_mode(ThinkingMode::Adaptive)
///     .with_effort(Effort::Medium);
///
/// // Budget-based thinking (legacy)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5")
///     .with_thinking_mode(ThinkingMode::Enabled { budget_tokens: 8192 });
///
/// // Prompt caching (enabled by default, can be disabled)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
///     .with_prompt_caching(false); // opt out
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// Anthropic API key.
    pub api_key: String,
    /// Model name (e.g., `"claude-sonnet-4-6"`, `"claude-opus-4-6"`).
    pub model: String,
    /// Maximum tokens to generate.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Optional custom base URL (for proxies, Ollama, Vercel Gateway, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Enable prompt caching with `cache_control` blocks.
    ///
    /// Defaults to `true`. Anthropic prompt caching reduces costs and latency
    /// by reusing previously processed context. Disable with
    /// [`with_prompt_caching(false)`](AnthropicConfig::with_prompt_caching).
    #[serde(default = "default_prompt_caching")]
    pub prompt_caching: bool,

    /// Thinking mode configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMode>,

    /// Effort level (goes into `output_config.effort`).
    /// Works with or without thinking enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,

    /// Enable fast mode (Opus 4.6 only, beta).
    /// Delivers up to 2.5× higher output tokens/sec at 6× pricing.
    #[serde(default)]
    pub fast_mode: bool,

    /// Enable citations on documents in requests.
    #[serde(default)]
    pub citations: bool,

    /// Geographic routing for data residency (e.g., `"US"`, `"EU"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<String>,

    /// Server-side context management (tool result clearing, thinking block clearing).
    /// Requires beta header (auto-injected by adk-anthropic).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<adk_anthropic::ContextManagement>,

    /// Service tier (`"auto"` or `"standard_only"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// Beta feature headers (e.g., `"prompt-caching-2024-07-31"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beta_features: Vec<String>,

    /// Custom API version header override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,

    /// Tool search configuration for regex-based dynamic tool discovery.
    /// When set, only tools whose names match the regex pattern are loaded.
    /// When `None`, all available tools are loaded.
    #[serde(skip)]
    pub tool_search: Option<ToolSearchConfig>,
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_prompt_caching() -> bool {
    true
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: default_max_tokens(),
            base_url: None,
            prompt_caching: true,
            thinking: None,
            effort: None,
            fast_mode: false,
            citations: false,
            inference_geo: None,
            context_management: None,
            service_tier: None,
            beta_features: Vec::new(),
            api_version: None,
            tool_search: None,
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
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled;
        self
    }

    /// Set the thinking mode.
    pub fn with_thinking_mode(mut self, mode: ThinkingMode) -> Self {
        self.thinking = Some(mode);
        self
    }

    /// Convenience: enable budget-based thinking with the given token budget.
    pub fn with_thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ThinkingMode::Enabled { budget_tokens });
        self
    }

    /// Set the effort level (goes into `output_config.effort`).
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Enable fast mode (Opus 4.6 only, beta).
    pub fn with_fast_mode(mut self, enabled: bool) -> Self {
        self.fast_mode = enabled;
        self
    }

    /// Enable citations on documents.
    pub fn with_citations(mut self, enabled: bool) -> Self {
        self.citations = enabled;
        self
    }

    /// Set geographic routing for data residency.
    pub fn with_inference_geo(mut self, geo: impl Into<String>) -> Self {
        self.inference_geo = Some(geo.into());
        self
    }

    /// Set the service tier.
    pub fn with_service_tier(mut self, tier: impl Into<String>) -> Self {
        self.service_tier = Some(tier.into());
        self
    }

    /// Set context management (tool result clearing, thinking block clearing).
    pub fn with_context_management(mut self, cm: adk_anthropic::ContextManagement) -> Self {
        self.context_management = Some(cm);
        self
    }

    /// Add a beta feature header value.
    pub fn with_beta_feature(mut self, feature: impl Into<String>) -> Self {
        self.beta_features.push(feature.into());
        self
    }

    /// Set a custom API version header.
    pub fn with_api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = Some(version.into());
        self
    }

    /// Set tool search configuration for dynamic tool discovery.
    ///
    /// When set, only tools whose names match the regex pattern are loaded per request.
    /// When not set, all available tools are loaded.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::anthropic::AnthropicConfig;
    /// use adk_anthropic::ToolSearchConfig;
    ///
    /// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
    ///     .with_tool_search(ToolSearchConfig::new("^(search|fetch)_.*"));
    /// ```
    pub fn with_tool_search(mut self, config: ToolSearchConfig) -> Self {
        self.tool_search = Some(config);
        self
    }
}
