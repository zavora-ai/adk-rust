//! Fireworks AI client implementation.

use super::config::{FIREWORKS_API_BASE, FireworksConfig};
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;

/// Fireworks AI client backed by the shared OpenAI-compatible implementation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::fireworks::{FireworksClient, FireworksConfig};
///
/// let config = FireworksConfig::new(
///     std::env::var("FIREWORKS_API_KEY").unwrap(),
///     "accounts/fireworks/models/llama-v3p1-8b-instruct",
/// );
/// let client = FireworksClient::new(config)?;
/// ```
pub struct FireworksClient {
    inner: OpenAICompatible,
}

impl FireworksClient {
    /// Create a new Fireworks AI client.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` if the underlying OpenAI-compatible client
    /// fails to initialize.
    pub fn new(config: FireworksConfig) -> Result<Self, AdkError> {
        let FireworksConfig { api_key, model, base_url } = config;
        let base_url = base_url.unwrap_or_else(|| FIREWORKS_API_BASE.to_string());
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("fireworks")
            .with_base_url(base_url);

        Ok(Self { inner: OpenAICompatible::new(config)? })
    }

    /// Set a retry configuration, consuming and returning `self`.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.inner = self.inner.with_retry_config(retry_config);
        self
    }

    /// Set a retry configuration by mutable reference.
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.inner.set_retry_config(retry_config);
    }

    /// Get the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for FireworksClient {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        self.inner.generate_content(request, stream).await
    }
}
