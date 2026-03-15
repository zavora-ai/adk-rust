//! OpenAI client implementation.

use super::config::{AzureConfig, OpenAIConfig};
use super::convert;
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_openai::{
    Client,
    config::AzureConfig as AsyncAzureConfig,
    types::chat::{
        CreateChatCompletionRequestArgs, ReasoningEffort, ResponseFormat, ResponseFormatJsonSchema,
    },
};
use async_stream::try_stream;
use async_trait::async_trait;

/// OpenAI client for standard OpenAI API and OpenAI-compatible APIs.
pub struct OpenAIClient {
    inner: OpenAICompatible,
}

impl OpenAIClient {
    /// Create a new OpenAI client.
    pub fn new(config: OpenAIConfig) -> Result<Self, AdkError> {
        let reasoning_effort = config.reasoning_effort.map(|e| match e {
            super::config::ReasoningEffort::Low => ReasoningEffort::Low,
            super::config::ReasoningEffort::Medium => ReasoningEffort::Medium,
            super::config::ReasoningEffort::High => ReasoningEffort::High,
        });

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
        if let Some(effort) = reasoning_effort {
            compat_config = compat_config.with_reasoning_effort(effort);
        }

        Ok(Self { inner: OpenAICompatible::new(compat_config)? })
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

    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.inner = self.inner.with_retry_config(retry_config);
        self
    }

    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.inner.set_retry_config(retry_config);
    }

    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for OpenAIClient {
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

/// Azure OpenAI client.
pub struct AzureOpenAIClient {
    client: Client<AsyncAzureConfig>,
    deployment_id: String,
    retry_config: RetryConfig,
}

impl AzureOpenAIClient {
    /// Create a new Azure OpenAI client.
    pub fn new(config: AzureConfig) -> Result<Self, AdkError> {
        let azure_config = AsyncAzureConfig::new()
            .with_api_base(&config.api_base)
            .with_api_version(&config.api_version)
            .with_deployment_id(&config.deployment_id)
            .with_api_key(&config.api_key);

        Ok(Self {
            client: Client::with_config(azure_config),
            deployment_id: config.deployment_id,
            retry_config: RetryConfig::default(),
        })
    }

    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}

#[async_trait]
impl Llm for AzureOpenAIClient {
    fn name(&self) -> &str {
        &self.deployment_id
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool, // Azure OpenAI always uses streaming internally
    ) -> Result<adk_core::LlmResponseStream, AdkError> {
        let deployment_id = self.deployment_id.clone();
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();

        let stream = try_stream! {
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let deployment_id = deployment_id.clone();
                let client = client.clone();
                let request = request_for_retry.clone();
                async move {
                    let messages: Vec<_> = request
                        .contents
                        .iter()
                        .map(convert::content_to_message)
                        .collect();

                    let mut request_builder = CreateChatCompletionRequestArgs::default();
                    request_builder.model(&deployment_id).messages(messages);

                    if !request.tools.is_empty() {
                        let tools = convert::convert_tools(&request.tools);
                        request_builder.tools(tools);
                    }

                    if let Some(config) = &request.config {
                        if let Some(temp) = config.temperature {
                            request_builder.temperature(temp);
                        }
                        if let Some(top_p) = config.top_p {
                            request_builder.top_p(top_p);
                        }
                        if let Some(max_tokens) = config.max_output_tokens {
                            request_builder.max_completion_tokens(max_tokens as u32);
                        }

                        if let Some(schema) = &config.response_schema {
                            let mut schema_with_strict = schema.clone();
                            if let Some(obj) = schema_with_strict.as_object_mut() {
                                obj.insert(
                                    "additionalProperties".to_string(),
                                    serde_json::json!(false),
                                );
                            }
                            let json_schema = ResponseFormatJsonSchema {
                                name: deployment_id.replace(['-', '.', '/'], "_"),
                                description: None,
                                schema: Some(schema_with_strict),
                                strict: Some(true),
                            };
                            request_builder
                                .response_format(ResponseFormat::JsonSchema { json_schema });
                        }
                    }

                    let openai_request = request_builder
                        .build()
                        .map_err(|e| AdkError::Model(format!("Failed to build request: {e}")))?;

                    // Use non-streaming create() — async-openai 0.33's create_stream() has a
                    // reqwest-eventsource compatibility bug. See openai_compatible.rs.
                    client
                        .chat()
                        .create(openai_request)
                        .await
                        .map_err(|e| AdkError::Model(format!("Azure OpenAI API error: {e}")))
                }
            })
            .await?;

            let adk_response = convert::from_openai_response(&response);
            yield adk_response;
        };

        Ok(Box::pin(stream))
    }
}
