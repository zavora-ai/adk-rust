//! Azure deployment routing over ADK's shared Chat Completions codec and stream parser.
use super::{AzureConfig, OpenAIReasoningEffort, OpenAiSchemaAdapter};
use crate::{
    openai_compatible::{ErrorCodes, OpenAICompatible, OpenAICompatibleConfig},
    retry::RetryConfig,
};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream, SchemaAdapter};
use async_trait::async_trait;
use std::sync::Arc;

/// The codes the dedicated Azure client reported before it shared the compatible transport.
const ERROR_CODES: ErrorCodes = ErrorCodes {
    request: "model.azure_openai.request",
    api_error: "model.azure_openai.api_error",
    parse: "model.azure_openai.parse",
    invalid_tool_arguments: "model.azure_openai.invalid_tool_arguments",
    status: &[
        (401, "model.azure_openai.unauthorized"),
        (404, "model.azure_openai.not_found"),
        (408, "model.azure_openai.timeout"),
        (429, "model.azure_openai.rate_limited"),
        (500, "model.azure_openai.unavailable"),
        (502, "model.azure_openai.unavailable"),
        (503, "model.azure_openai.unavailable"),
        (504, "model.azure_openai.unavailable"),
        (529, "model.azure_openai.overloaded"),
    ],
};

/// Azure deployment client with API-key authentication and streaming support.
pub struct AzureOpenAIClient {
    inner: OpenAICompatible,
}

impl AzureOpenAIClient {
    /// Construct a client from explicit deployment routing and credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when `api_base` is not an absolute URL or `api_key` cannot be
    /// sent as an HTTP header value.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::openai::{AzureConfig, AzureOpenAIClient};
    ///
    /// let config = AzureConfig::new(
    ///     "azure-key",
    ///     "https://my-resource.openai.azure.com",
    ///     "2024-12-01-preview",
    ///     "gpt-4o",
    /// );
    /// let client = AzureOpenAIClient::new(config)?;
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn new(config: AzureConfig) -> Result<Self, AdkError> {
        let mut url = reqwest::Url::parse(&config.api_base)
            .map_err(|_| AdkError::model("invalid Azure endpoint"))?;
        url.path_segments_mut()
            .map_err(|_| AdkError::model("invalid Azure endpoint"))?
            .pop_if_empty()
            .extend(["openai", "deployments", &config.deployment_id, "chat", "completions"]);
        url.query_pairs_mut().append_pair("api-version", &config.api_version);
        let mut key = reqwest::header::HeaderValue::from_str(&config.api_key)
            .map_err(|_| AdkError::model("invalid Azure API key"))?;
        key.set_sensitive(true);
        let inner = OpenAICompatible::new(
            OpenAICompatibleConfig::new("", config.deployment_id)
                .with_provider_name("azure-openai"),
        )?
        .with_completion_url(url.to_string())
        .with_error_codes(ERROR_CODES)
        .with_request_adapter(Arc::new(move |_, headers| {
            headers.remove(reqwest::header::AUTHORIZATION);
            headers.insert("api-key", key.clone());
            Ok(())
        }));
        Ok(Self { inner })
    }

    /// Set the deployment's supported reasoning effort.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::openai::{AzureConfig, AzureOpenAIClient, OpenAIReasoningEffort};
    ///
    /// let config = AzureConfig::new(
    ///     "azure-key",
    ///     "https://my-resource.openai.azure.com",
    ///     "2024-12-01-preview",
    ///     "o4-mini",
    /// );
    /// let client = AzureOpenAIClient::new(config)?
    ///     .with_reasoning_effort(Some(OpenAIReasoningEffort::Low));
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: Option<OpenAIReasoningEffort>) -> Self {
        self.inner = self.inner.with_reasoning_effort(effort);
        self
    }

    /// Set the retry policy while constructing the client.
    #[must_use]
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.inner.set_retry_config(config);
        self
    }

    /// Replace the retry policy.
    pub fn set_retry_config(&mut self, config: RetryConfig) {
        self.inner.set_retry_config(config);
    }

    /// Return the configured retry policy.
    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for AzureOpenAIClient {
    fn max_cost_micro_usd(&self, request: &adk_core::LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        static ADAPTER: OpenAiSchemaAdapter = OpenAiSchemaAdapter;
        &ADAPTER
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        self.inner.generate_content(request, stream).await
    }
}
