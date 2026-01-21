//! Configuration types for OpenAI providers.

use serde::{Deserialize, Serialize};

/// Configuration for OpenAI API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    /// OpenAI API key.
    pub api_key: String,
    /// Model name (e.g., "gpt-4o", "gpt-4o-mini", "gpt-4-turbo").
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
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            organization_id: None,
            project_id: None,
            base_url: None,
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
