//! OpenAI client implementation.

use super::config::{OpenAIConfig, OpenAIReasoningEffort};
use super::schema_adapter::OpenAiSchemaAdapter;
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponseStream, SchemaAdapter,
};
use async_trait::async_trait;

/// OpenAI client for standard OpenAI API and OpenAI-compatible APIs.
pub struct OpenAIClient {
    inner: OpenAICompatible,
}

impl OpenAIClient {
    /// Create a new OpenAI client.
    pub fn new(config: OpenAIConfig) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("openai", &config.model);
        let reasoning_effort = config.reasoning_effort.map(OpenAIReasoningEffort::from);

        Self::new_inner(config, reasoning_effort)
    }

    /// Create a client with the complete OpenAI reasoning-effort vocabulary.
    ///
    /// Use this constructor for newer Chat Completions values such as
    /// [`OpenAIReasoningEffort::None`] or [`OpenAIReasoningEffort::XHigh`].
    /// [`OpenAIReasoningEffort::Max`] is supported by the Responses API, not
    /// Chat Completions. The original [`OpenAIConfig::reasoning_effort`] field
    /// remains available for backward compatibility.
    pub fn new_with_reasoning_effort(
        config: OpenAIConfig,
        reasoning_effort: OpenAIReasoningEffort,
    ) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("openai", &config.model);
        if reasoning_effort == OpenAIReasoningEffort::Max {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openai.reasoning_effort_unsupported",
                "OpenAI Chat Completions does not support reasoning effort `max`; use OpenAIResponsesClient for `max`, or use `xhigh` with OpenAIClient",
            )
            .with_provider("openai"));
        }
        Self::new_inner(config, Some(reasoning_effort))
    }

    fn new_inner(
        config: OpenAIConfig,
        reasoning_effort: Option<OpenAIReasoningEffort>,
    ) -> Result<Self, AdkError> {
        let mut compat_config =
            OpenAICompatibleConfig::new(config.api_key, config.model).with_provider_name("openai");
        if let Some(base_url) = config.base_url {
            compat_config = compat_config.with_base_url(base_url);
        }
        if let Some(org_id) = config.organization_id {
            compat_config = compat_config.with_organization(org_id);
        }
        if let Some(project_id) = config.project_id {
            compat_config = compat_config.with_project(project_id);
        }
        Ok(Self {
            inner: OpenAICompatible::new_with_reasoning_effort(compat_config, reasoning_effort)?,
        })
    }

    /// Create a client for an OpenAI-compatible API.
    pub fn compatible(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, AdkError> {
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("openai-compatible")
            .with_base_url(base_url);
        Ok(Self { inner: OpenAICompatible::new(config)? })
    }

    /// Set the retry configuration (builder pattern).
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.inner = self.inner.with_retry_config(retry_config);
        self
    }

    /// Set the retry configuration (mutable reference).
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.inner.set_retry_config(retry_config);
    }

    /// Returns the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for OpenAIClient {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        static ADAPTER: OpenAiSchemaAdapter = OpenAiSchemaAdapter;
        &ADAPTER
    }

    #[tracing::instrument(
        name = "model.generate_content",
        skip_all,
        fields(
            model.name = %self.name(),
            stream = %stream,
            request.contents_count = %request.contents.len(),
            request.tools_count = %request.tools.len()
        )
    )]
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        self.inner.generate_content(request, stream).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_rejects_responses_only_max_effort() {
        let config = OpenAIConfig::new("test-key", "gpt-5.6-terra");
        let error =
            match OpenAIClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max) {
                Ok(_) => panic!("Chat Completions must reject max reasoning effort"),
                Err(error) => error,
            };

        assert_eq!(error.code, "model.openai.reasoning_effort_unsupported");
        assert!(error.to_string().contains("OpenAIResponsesClient"));
    }
}
