//! Configuration types for Anthropic provider.

use adk_anthropic::ToolSearchConfig;
use serde::{Deserialize, Serialize};

/// Thinking mode configuration for Anthropic models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingMode {
    /// Budget-based thinking (legacy, deprecated on 4.6 models,
    /// **rejected on Opus 4.7**).
    /// Requires `budget_tokens` < `max_tokens`.
    Enabled {
        /// Token budget for thinking (must be ≥ 1024).
        budget_tokens: u32,
    },
    /// Adaptive thinking for Opus 4.7 / Opus 4.6 / Sonnet 4.6.
    /// Claude decides when and how much to think.
    /// Control depth via `effort` on `AnthropicConfig`.
    Adaptive,
}

/// Effort level controlling response thoroughness.
/// Passed via `output_config.effort` in the API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Minimal reasoning effort.
    Low,
    /// Moderate reasoning effort.
    Medium,
    /// Thorough reasoning effort.
    High,
    /// Very deep reasoning. Opus 4.7+ recommended.
    XHigh,
    /// Opus 4.6+ only.
    Max,
}

/// Configuration for Anthropic API.
///
/// # Example
///
/// ```rust
/// use adk_model::anthropic::{AnthropicConfig, ThinkingMode, Effort};
///
/// // Opus 4.7 with adaptive thinking and xhigh effort (recommended)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-opus-4-7")
///     .with_thinking_mode(ThinkingMode::Adaptive)
///     .with_effort(Effort::XHigh);
///
/// // Adaptive thinking with medium effort (recommended for Sonnet 4.6)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
///     .with_thinking_mode(ThinkingMode::Adaptive)
///     .with_effort(Effort::Medium);
///
/// // Budget-based thinking (legacy, rejected on Opus 4.7)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5")
///     .with_thinking_mode(ThinkingMode::Enabled { budget_tokens: 8192 });
///
/// // Prompt caching (enabled by default, can be disabled)
/// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-6")
///     .with_prompt_caching(false); // opt out
/// ```
///
/// The API key is redacted from `Debug` output and omitted when serializing.
#[derive(Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// Anthropic API key. Never serialized; deserializing a config without it
    /// yields an empty key.
    #[serde(skip_serializing, default)]
    pub api_key: String,
    /// Model name (e.g., `"claude-opus-5"`, `"claude-sonnet-5"`).
    pub model: String,
    /// Maximum tokens to generate. `None` uses
    /// [`anthropic_default_max_tokens`](crate::catalog::anthropic_default_max_tokens)
    /// for the model: 32,000 for Claude 4 and later, whose thinking counts toward
    /// this cap, and 4,096 for older or unrecognized models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Bound on a whole non-streaming request, and on the wait for the response
    /// headers of a streaming one, in seconds. `None` uses 600 (10 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<u64>,
    /// Optional custom base URL (for proxies, Ollama, Vercel Gateway, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Accept a plain `http://` [`base_url`](AnthropicConfig::base_url) on a
    /// non-loopback host.
    ///
    /// Defaults to `false`. Set with
    /// [`allow_insecure_http`](AnthropicConfig::allow_insecure_http).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_insecure_http: bool,

    /// Enable prompt caching with `cache_control` blocks.
    ///
    /// Defaults to `true`. Anthropic prompt caching reduces costs and latency
    /// by reusing previously processed context. Disable with
    /// [`with_prompt_caching(false)`](AnthropicConfig::with_prompt_caching).
    #[serde(default = "default_prompt_caching")]
    pub prompt_caching: bool,

    /// Lifetime of the prompt cache entries this client writes. `None` uses the
    /// API default of 5 minutes. Set with
    /// [`with_prompt_cache_ttl`](AnthropicConfig::with_prompt_cache_ttl).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl: Option<adk_anthropic::CacheTtl>,

    /// Thinking mode configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMode>,

    /// Effort level (goes into `output_config.effort`).
    /// Works with or without thinking enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,

    /// Enable fast mode for Claude Opus 5 or Claude Opus 4.8 (research preview).
    /// Delivers up to 2.5× higher output tokens/sec at premium pricing.
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

impl std::fmt::Debug for AnthropicConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicConfig")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("request_timeout_secs", &self.request_timeout_secs)
            .field("base_url", &self.base_url)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .field("prompt_caching", &self.prompt_caching)
            .field("prompt_cache_ttl", &self.prompt_cache_ttl)
            .field("thinking", &self.thinking)
            .field("effort", &self.effort)
            .field("fast_mode", &self.fast_mode)
            .field("citations", &self.citations)
            .field("inference_geo", &self.inference_geo)
            .field("context_management", &self.context_management)
            .field("service_tier", &self.service_tier)
            .field("beta_features", &self.beta_features)
            .field("api_version", &self.api_version)
            .field("tool_search", &self.tool_search)
            .finish()
    }
}

fn default_prompt_caching() -> bool {
    true
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: crate::catalog::ANTHROPIC_DEFAULT.to_string(),
            max_tokens: None,
            request_timeout_secs: None,
            base_url: None,
            allow_insecure_http: false,
            prompt_caching: true,
            prompt_cache_ttl: None,
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

    /// Set the maximum tokens to generate, replacing the model-aware default.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Bound each request, replacing the 10-minute default.
    ///
    /// The bound covers a whole non-streaming request, such as an agent called
    /// through `AgentTool`, and the wait for the response headers of a streaming
    /// request. Sub-second precision is rounded up to the next second.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use adk_model::anthropic::AnthropicConfig;
    ///
    /// let config = AnthropicConfig::new("sk-ant-xxx", "claude-opus-5-5")
    ///     .with_request_timeout(Duration::from_secs(1_800));
    /// assert_eq!(config.request_timeout_secs, Some(1_800));
    /// ```
    pub fn with_request_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.request_timeout_secs = Some(timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0));
        self
    }

    /// Set a custom base URL.
    ///
    /// The URL is validated when [`AnthropicClient::new`](super::AnthropicClient::new)
    /// builds the client: it must use `https://`, or `http://` with a loopback host,
    /// unless [`allow_insecure_http`](Self::allow_insecure_http) is set.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Allow a plain `http://` base URL on a non-loopback host.
    ///
    /// Use this for a trusted internal gateway that is reachable only over plain
    /// HTTP. Every request attaches the API key, which then crosses the network
    /// unencrypted. The base URL is validated when
    /// [`AnthropicClient::new`](super::AnthropicClient::new) builds the client, so
    /// this method and [`with_base_url`](Self::with_base_url) can be called in
    /// either order. Accepting a non-loopback `http://` URL logs a warning with
    /// the host.
    ///
    /// The opt-in applies to the configured base URL. Without one, the client
    /// reads `ANTHROPIC_BASE_URL`, and a plain-HTTP URL from that variable is
    /// acknowledged by `ANTHROPIC_ALLOW_INSECURE_HTTP=1` instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
    ///
    /// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-5")
    ///     .allow_insecure_http()
    ///     .with_base_url("http://10.60.1.20:8080/api/v1/llm/anthropic");
    /// let client = AnthropicClient::new(config)?;
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn allow_insecure_http(mut self) -> Self {
        self.allow_insecure_http = true;
        self
    }

    /// Enable or disable prompt caching.
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled;
        self
    }

    /// Set the lifetime of the prompt cache entries this client writes.
    ///
    /// Applies to both the system-prompt breakpoint and the automatic breakpoint
    /// on the conversation tail. A 1-hour entry costs more to write than a
    /// 5-minute one and pays off when requests sharing a prefix are more than five
    /// minutes apart, such as a conversation waiting on a person.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::anthropic::{AnthropicConfig, CacheTtl};
    ///
    /// let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-5-5")
    ///     .with_prompt_cache_ttl(CacheTtl::one_hour());
    /// assert_eq!(config.prompt_cache_ttl, Some(CacheTtl::one_hour()));
    /// ```
    pub fn with_prompt_cache_ttl(mut self, ttl: adk_anthropic::CacheTtl) -> Self {
        self.prompt_cache_ttl = Some(ttl);
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

    /// Enable fast mode for Claude Opus 5 or Claude Opus 4.8.
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
