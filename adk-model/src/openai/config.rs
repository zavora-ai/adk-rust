//! Configuration types for OpenAI providers.

use serde::{Deserialize, Serialize};

/// Reasoning effort level for OpenAI reasoning models (e.g., o1, o3).
///
/// Controls how much reasoning effort the model applies. Maps directly to
/// the OpenAI `reasoning_effort` API field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Minimal reasoning — fastest, cheapest.
    Low,
    /// Balanced reasoning (default for most reasoning models).
    Medium,
    /// Maximum reasoning — most thorough but slowest.
    High,
}

/// Complete reasoning-effort vocabulary supported across OpenAI model generations.
///
/// This additive type preserves the original exhaustive [`ReasoningEffort`] enum while
/// exposing the `none`, `minimal`, `xhigh`, and `max` values used by newer models.
/// GPT-5.6 supports every value except `minimal` through the Responses API;
/// Chat Completions supports up to `xhigh`. Older GPT-5 models may support
/// `minimal` but not `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAIReasoningEffort {
    /// Disable reasoning for the lowest latency.
    None,
    /// Use minimal reasoning on models that retain the legacy setting.
    Minimal,
    /// Use low reasoning effort.
    Low,
    /// Use balanced reasoning effort.
    Medium,
    /// Use high reasoning effort.
    High,
    /// Use extra-high reasoning effort.
    XHigh,
    /// Use maximum reasoning effort on models that support it through Responses.
    Max,
}

impl From<ReasoningEffort> for OpenAIReasoningEffort {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
        }
    }
}

/// Configuration for OpenAI API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    /// OpenAI API key.
    pub api_key: String,
    /// Model name (e.g., "gpt-5.6-terra", "gpt-5.6-sol").
    pub model: String,
    /// Optional organization ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Optional project ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Optional custom base URL for OpenAI-compatible APIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Reasoning effort for OpenAI reasoning models (o1, o3, etc.).
    ///
    /// When set, the `reasoning_effort` field is included in the API request.
    /// Only applicable to reasoning-capable models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: crate::catalog::OPENAI_DEFAULT.to_string(),
            organization_id: None,
            project_id: None,
            base_url: None,
            reasoning_effort: None,
        }
    }
}

impl OpenAIConfig {
    /// Create a new OpenAI config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Create a config for an OpenAI-compatible API (e.g., Ollama, vLLM).
    pub fn compatible(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: Some(base_url.into()),
            ..Default::default()
        }
    }

    /// Set the organization ID.
    pub fn with_organization(mut self, org_id: impl Into<String>) -> Self {
        self.organization_id = Some(org_id.into());
        self
    }

    /// Set the project ID.
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Set the reasoning effort for reasoning models (o1, o3, etc.).
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }
}

/// Configuration for Azure OpenAI Service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureConfig {
    /// Azure OpenAI API key.
    pub api_key: String,
    /// Azure resource endpoint (e.g., `https://my-resource.openai.azure.com`).
    pub api_base: String,
    /// API version (e.g., "2024-02-15-preview").
    pub api_version: String,
    /// Deployment name/ID.
    pub deployment_id: String,
}

impl AzureConfig {
    /// Create a new Azure OpenAI config.
    pub fn new(
        api_key: impl Into<String>,
        api_base: impl Into<String>,
        api_version: impl Into<String>,
        deployment_id: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            api_base: api_base.into(),
            api_version: api_version.into(),
            deployment_id: deployment_id.into(),
        }
    }
}

/// Transport mode for the Responses API.
///
/// Controls whether the client uses standard HTTP/SSE or a persistent
/// WebSocket connection for lower-latency agentic workflows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponsesTransport {
    /// Standard HTTP/SSE (default).
    #[default]
    Http,
    /// Persistent WebSocket connection (requires `openai-ws` feature).
    WebSocket,
}

/// Service tier for processing priority.
///
/// Controls the processing priority for API requests. Priority tier
/// provides lower latency and more consistent token generation speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    /// Automatic tier selection (default behavior).
    Auto,
    /// Priority processing for lower latency.
    Priority,
}

/// Prompt cache retention duration.
///
/// Controls how long prompt prefixes are cached for repeated requests.
/// Caching reduces costs for requests that share the same prompt prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheRetention {
    /// In-memory cache (shortest retention, lowest cost).
    InMemory,
    /// 24-hour cache retention.
    #[serde(rename = "24h")]
    TwentyFourHours,
}

/// Reasoning summary mode for the Responses API.
///
/// Controls whether and how the model generates a summary of its internal
/// reasoning process. Only applicable to o-series reasoning models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    /// Let the model decide whether to include a summary.
    Auto,
    /// Include a brief summary of the reasoning.
    Concise,
    /// Include a thorough summary of the reasoning.
    Detailed,
}

/// Configuration for the OpenAI Responses API client.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::openai::{OpenAIResponsesConfig, ReasoningEffort, ReasoningSummary};
///
/// let config = OpenAIResponsesConfig::new("sk-...", "o3")
///     .with_reasoning_effort(ReasoningEffort::High)
///     .with_reasoning_summary(ReasoningSummary::Concise);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIResponsesConfig {
    /// OpenAI API key.
    pub api_key: String,
    /// Model name (e.g., "o3", "o4-mini", "gpt-4.1").
    pub model: String,
    /// Optional organization ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Optional project ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Reasoning effort for o-series models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Reasoning summary mode for o-series models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummary>,
    /// Transport mode (HTTP or WebSocket).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<ResponsesTransport>,
    /// Default service tier for processing priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Default prompt cache retention policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<PromptCacheRetention>,
    /// Enable Open Responses mode for third-party compatibility.
    ///
    /// When enabled, relaxes strict OpenAI field validation to support
    /// Open Responses-compatible endpoints (LM Studio, Ollama, vLLM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_responses_mode: Option<bool>,
}

impl OpenAIResponsesConfig {
    /// Create a new Responses API config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            organization_id: None,
            project_id: None,
            base_url: None,
            reasoning_effort: None,
            reasoning_summary: None,
            transport: None,
            service_tier: None,
            prompt_cache_retention: None,
            open_responses_mode: None,
        }
    }

    /// Set the organization ID.
    #[must_use]
    pub fn with_organization(mut self, org_id: impl Into<String>) -> Self {
        self.organization_id = Some(org_id.into());
        self
    }

    /// Set the project ID.
    #[must_use]
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Set the base URL.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Set the reasoning effort for o-series models.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Set the reasoning summary mode for o-series models.
    #[must_use]
    pub fn with_reasoning_summary(mut self, summary: ReasoningSummary) -> Self {
        self.reasoning_summary = Some(summary);
        self
    }

    /// Set the transport mode (HTTP or WebSocket).
    ///
    /// Defaults to HTTP when not set. WebSocket requires the `openai-ws` feature.
    #[must_use]
    pub fn with_transport(mut self, transport: ResponsesTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Set the default service tier for processing priority.
    ///
    /// Priority tier provides lower latency and more consistent token generation.
    #[must_use]
    pub fn with_service_tier(mut self, tier: ServiceTier) -> Self {
        self.service_tier = Some(tier);
        self
    }

    /// Set the default prompt cache retention policy.
    ///
    /// Controls how long prompt prefixes are cached for cost optimization.
    #[must_use]
    pub fn with_prompt_cache_retention(mut self, retention: PromptCacheRetention) -> Self {
        self.prompt_cache_retention = Some(retention);
        self
    }

    /// Enable or disable Open Responses mode.
    ///
    /// When enabled, relaxes strict OpenAI field validation for compatibility
    /// with third-party Open Responses-compatible endpoints.
    #[must_use]
    pub fn with_open_responses_mode(mut self, enabled: bool) -> Self {
        self.open_responses_mode = Some(enabled);
        self
    }
}
