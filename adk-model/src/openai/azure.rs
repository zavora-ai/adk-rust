//! Azure deployment routing over ADK's shared Chat Completions codec and stream parser.
use super::{AzureConfig, OpenAIReasoningEffort, OpenAiSchemaAdapter};
use crate::{
    openai_compatible::{OpenAICompatible, OpenAICompatibleConfig},
    retry::RetryConfig,
};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream, SchemaAdapter};
use async_trait::async_trait;
use std::sync::Arc;

/// Azure deployment client with API-key authentication and streaming support.
pub struct AzureOpenAIClient {
    inner: OpenAICompatible,
}

impl AzureOpenAIClient {
    /// Construct a client from explicit deployment routing and credentials.
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
        .with_request_adapter(Arc::new(move |_, headers| {
            headers.remove(reqwest::header::AUTHORIZATION);
            headers.insert("api-key", key.clone());
            Ok(())
        }));
        Ok(Self { inner })
    }

    /// Set the deployment's supported reasoning effort.
    pub fn with_reasoning_effort(mut self, effort: Option<OpenAIReasoningEffort>) -> Self {
        self.inner = self.inner.with_reasoning_effort(effort);
        self
    }

    /// Set the retry policy while constructing the client.
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
