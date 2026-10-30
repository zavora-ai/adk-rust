//! Configuration types for the Meta Muse provider.

use serde::{Deserialize, Serialize};

/// Default Meta Model API base URL.
pub const MUSE_API_BASE: &str = "https://api.meta.ai/v1";

/// Minimum output cap the client sends. Spark reasoning counts toward `max_tokens` —
/// a trivial greeting was observed spending over 500 reasoning tokens — so smaller
/// caps are raised to this instead of returning empty `length` responses.
pub const MUSE_MIN_MAX_TOKENS: u32 = 1024;

/// Configuration for the Meta Model API.
///
/// The key is read from `META_API_KEY` (Meta also documents `MODEL_API_KEY`); see the
/// [module documentation](crate::muse) for an example.
///
/// The API key is redacted from `Debug` output and omitted when serializing.
#[derive(Clone, Serialize, Deserialize)]
pub struct MuseConfig {
    /// Meta Model API key. Never serialized; deserializing a config without it yields an empty key.
    #[serde(skip_serializing, default)]
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Maximum tokens for output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl std::fmt::Debug for MuseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuseConfig")
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

impl Default for MuseConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: crate::catalog::MUSE_DEFAULT.to_string(),
            base_url: None,
            max_tokens: None,
        }
    }
}

impl MuseConfig {
    /// Create a new Muse config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    /// Create a config for `muse-spark-1.3`, the recommended default.
    pub fn spark_1_3(api_key: impl Into<String>) -> Self {
        Self::new(api_key, crate::catalog::MUSE_DEFAULT)
    }

    /// Create a config for `muse-spark-1.2`, the previous version.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::muse::MuseConfig;
    ///
    /// let config = MuseConfig::spark_1_2("LLM-key");
    /// assert_eq!(config.model, "muse-spark-1.2");
    /// ```
    pub fn spark_1_2(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "muse-spark-1.2")
    }

    /// Create a config for `muse-spark-1.1`, the original version.
    pub fn spark_1_1(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "muse-spark-1.1")
    }

    /// Create a config for `muse-wonder-1.0`, the flagship (preview) model.
    pub fn wonder(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "muse-wonder-1.0")
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
        self.base_url.as_deref().unwrap_or(MUSE_API_BASE)
    }
}

#[cfg(test)]
mod tests {
    use super::MuseConfig;
    use crate::catalog::{ModelLifecycle, lookup_model};

    #[test]
    fn presets_select_active_catalog_models() {
        for config in [
            MuseConfig::spark_1_3("LLM-key"),
            MuseConfig::spark_1_2("LLM-key"),
            MuseConfig::spark_1_1("LLM-key"),
        ] {
            let entry = lookup_model("muse", &config.model)
                .unwrap_or_else(|| panic!("{} is not in the catalog", config.model));
            assert_eq!(entry.lifecycle, ModelLifecycle::Active, "{}", config.model);
        }
    }

    #[test]
    fn debug_redacts_the_api_key() {
        let rendered = format!("{:?}", MuseConfig::spark_1_3("LLM-secret"));
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("LLM-secret"));
    }
}
