//! Together AI client implementation.

use super::config::{TOGETHER_API_BASE, TogetherConfig};
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;

/// Together AI client backed by the shared OpenAI-compatible implementation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::together::{TogetherClient, TogetherConfig};
///
/// let config = TogetherConfig::new(
///     std::env::var("TOGETHER_API_KEY").unwrap(),
///     "meta-llama/Llama-3.3-70B-Instruct-Turbo",
/// );
/// let client = TogetherClient::new(config)?;
/// ```
pub struct TogetherClient {
    inner: OpenAICompatible,
}

impl TogetherClient {
    /// Create a new Together AI client.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` if the underlying OpenAI-compatible client
    /// fails to initialize.
    pub fn new(config: TogetherConfig) -> Result<Self, AdkError> {
        let TogetherConfig { api_key, model, base_url } = config;
        let base_url = base_url.unwrap_or_else(|| TOGETHER_API_BASE.to_string());
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("together")
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
impl Llm for TogetherClient {
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
