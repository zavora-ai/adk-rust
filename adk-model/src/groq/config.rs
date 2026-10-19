//! Configuration types for Groq provider.

use serde::{Deserialize, Serialize};

/// Default Groq API base URL.
pub const GROQ_API_BASE: &str = "https://api.groq.com/openai/v1";

/// Configuration for Groq API.
///
/// The API key is redacted from `Debug` output and omitted when serializing.
#[derive(Clone, Serialize, Deserialize)]
pub struct GroqConfig {
    /// Groq API key. Never serialized; deserializing a config without it yields an empty key.
    #[serde(skip_serializing, default)]
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

impl std::fmt::Debug for GroqConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroqConfig")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("reasoning_enabled", &self.reasoning_enabled)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

impl Default for GroqConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: crate::catalog::GROQ_DEFAULT.to_string(),
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

    /// Create a config for the legacy `llama-3.3-70b-versatile` model.
    ///
    /// This model was removed from Groq's free and developer tiers on
    /// 2026-08-16. Enterprise committed-spend plans may retain access. Prefer
    /// [`GroqConfig::gpt_oss_120b`] for portable new deployments.
    pub fn llama70b(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "llama-3.3-70b-versatile")
    }

    /// Create a config for Groq's production `openai/gpt-oss-120b` model.
    pub fn gpt_oss_120b(api_key: impl Into<String>) -> Self {
        Self::new(api_key, crate::catalog::GROQ_DEFAULT)
    }

    /// Create a config for Groq's production `openai/gpt-oss-20b` model, the faster,
    /// lower-cost choice.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::groq::GroqConfig;
    ///
    /// let config = GroqConfig::gpt_oss_20b("gsk-key");
    /// assert_eq!(config.model, "openai/gpt-oss-20b");
    /// ```
    pub fn gpt_oss_20b(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "openai/gpt-oss-20b")
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

#[cfg(test)]
mod tests {
    use super::GroqConfig;
    use crate::catalog::{ModelLifecycle, lookup_model};

    #[test]
    fn presets_select_active_catalog_models() {
        for config in [GroqConfig::gpt_oss_120b("gsk-key"), GroqConfig::gpt_oss_20b("gsk-key")] {
            let entry = lookup_model("groq", &config.model)
                .unwrap_or_else(|| panic!("{} is not in the catalog", config.model));
            assert_eq!(entry.lifecycle, ModelLifecycle::Active, "{}", config.model);
        }
    }
}
