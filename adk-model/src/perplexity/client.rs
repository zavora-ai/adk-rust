//! Perplexity client implementation.

use super::config::{PERPLEXITY_API_BASE, PerplexityConfig};
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;

/// Perplexity client backed by the shared OpenAI-compatible implementation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::perplexity::{PerplexityClient, PerplexityConfig};
///
/// let config = PerplexityConfig::new(
///     std::env::var("PERPLEXITY_API_KEY").unwrap(),
///     "sonar",
/// );
/// let client = PerplexityClient::new(config)?;
/// ```
pub struct PerplexityClient {
    inner: OpenAICompatible,
}

impl PerplexityClient {
    /// Create a new Perplexity client.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` if the underlying OpenAI-compatible client
    /// fails to initialize.
    pub fn new(config: PerplexityConfig) -> Result<Self, AdkError> {
        let PerplexityConfig { api_key, model, base_url } = config;
        let base_url = base_url.unwrap_or_else(|| PERPLEXITY_API_BASE.to_string());
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("perplexity")
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
impl Llm for PerplexityClient {
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
