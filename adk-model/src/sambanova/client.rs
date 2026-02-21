//! SambaNova client implementation.

use super::config::{SAMBANOVA_API_BASE, SambaNovaConfig};
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;

/// SambaNova client backed by the shared OpenAI-compatible implementation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::sambanova::{SambaNovaClient, SambaNovaConfig};
///
/// let config = SambaNovaConfig::new(
///     std::env::var("SAMBANOVA_API_KEY").unwrap(),
///     "Meta-Llama-3.3-70B-Instruct",
/// );
/// let client = SambaNovaClient::new(config)?;
/// ```
pub struct SambaNovaClient {
    inner: OpenAICompatible,
}

impl SambaNovaClient {
    /// Create a new SambaNova client.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Model` if the underlying OpenAI-compatible client
    /// fails to initialize.
    pub fn new(config: SambaNovaConfig) -> Result<Self, AdkError> {
        let SambaNovaConfig { api_key, model, base_url } = config;
        let base_url = base_url.unwrap_or_else(|| SAMBANOVA_API_BASE.to_string());
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("sambanova")
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
impl Llm for SambaNovaClient {
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
