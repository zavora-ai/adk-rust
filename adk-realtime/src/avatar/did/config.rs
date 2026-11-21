//! D-ID provider configuration.
//!
//! Contains [`DIDConfig`] and [`DIDLlmConfig`] for configuring the D-ID
//! Realtime Agents avatar provider.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

/// Custom LLM configuration for D-ID agents.
///
/// When provided, overrides the D-ID agent's default LLM settings.
///
/// # Example
///
/// ```rust
/// use adk_realtime::avatar::did::DIDLlmConfig;
///
/// let llm = DIDLlmConfig {
///     provider: "openai".to_string(),
///     model: "gpt-6.1-sol".to_string(),
///     instructions: Some("You are a helpful assistant.".to_string()),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DIDLlmConfig {
    /// LLM provider (e.g., "openai", "anthropic").
    pub provider: String,
    /// Model identifier.
    pub model: String,
    /// System instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Configuration for the D-ID avatar provider.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::avatar::did::DIDConfig;
///
/// let config = DIDConfig::new("your-api-key", "your-agent-id")
///     .with_base_url("https://api.d-id.com")
///     .with_knowledge_id("kb-123");
/// ```
pub struct DIDConfig {
    /// D-ID API key.
    pub api_key: SecretString,
    /// D-ID API base URL (default: `https://api.d-id.com`).
    pub api_base_url: String,
    /// D-ID agent ID to use for sessions.
    pub agent_id: String,
    /// Optional custom LLM configuration.
    pub llm_config: Option<DIDLlmConfig>,
    /// Optional knowledge base ID.
    pub knowledge_id: Option<String>,
}

impl DIDConfig {
    /// Create a new `DIDConfig` with the given API key and agent ID, plus sensible defaults.
    ///
    /// Defaults:
    /// - `api_base_url`: `https://api.d-id.com`
    /// - `llm_config`: `None`
    /// - `knowledge_id`: `None`
    pub fn new(api_key: impl Into<String>, agent_id: impl Into<String>) -> Self {
        Self {
            api_key: SecretString::from(api_key.into()),
            api_base_url: "https://api.d-id.com".to_string(),
            agent_id: agent_id.into(),
            llm_config: None,
            knowledge_id: None,
        }
    }

    /// Set a custom LLM configuration for the D-ID agent.
    pub fn with_llm_config(mut self, llm_config: DIDLlmConfig) -> Self {
        self.llm_config = Some(llm_config);
        self
    }

    /// Set a knowledge base ID.
    pub fn with_knowledge_id(mut self, knowledge_id: impl Into<String>) -> Self {
        self.knowledge_id = Some(knowledge_id.into());
        self
    }

    /// Set a custom API base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.api_base_url = url.into();
        self
    }
}

// Custom Debug that redacts the API key.
impl std::fmt::Debug for DIDConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DIDConfig")
            .field("api_key", &"[REDACTED]")
            .field("api_base_url", &self.api_base_url)
            .field("agent_id", &self.agent_id)
            .field("llm_config", &self.llm_config)
            .field("knowledge_id", &self.knowledge_id)
            .finish()
    }
}
