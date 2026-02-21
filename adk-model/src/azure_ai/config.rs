//! Configuration types for Azure AI Inference provider.

use serde::{Deserialize, Serialize};

/// Configuration for Azure AI Inference endpoints.
///
/// Azure AI Inference uses a dedicated endpoint URL and API key for
/// authentication, rather than a shared base URL. Each endpoint hosts
/// a specific model deployment (e.g., Cohere, Llama, Mistral).
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::azure_ai::AzureAIConfig;
///
/// let config = AzureAIConfig::new(
///     "https://my-endpoint.eastus.inference.ai.azure.com",
///     "my-api-key",
///     "meta-llama-3.1-8b-instruct",
/// );
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureAIConfig {
    /// Azure AI Inference endpoint URL (e.g.,
    /// `"https://my-endpoint.eastus.inference.ai.azure.com"`).
    pub endpoint: String,
    /// Azure API key for the endpoint.
    pub api_key: String,
    /// Model name deployed at the endpoint.
    pub model: String,
}

impl AzureAIConfig {
    /// Create a new Azure AI config with the given endpoint, API key, and model.
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self { endpoint: endpoint.into(), api_key: api_key.into(), model: model.into() }
    }
}
