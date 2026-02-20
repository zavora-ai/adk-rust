//! Shared OpenAI-compatible provider implementation.

use crate::openai::convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream, Part};
use async_openai::{
    Client,
    config::OpenAIConfig as AsyncOpenAIConfig,
    types::{CreateChatCompletionRequestArgs, ResponseFormat, ResponseFormatJsonSchema},
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

/// Configuration for OpenAI-compatible providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAICompatibleConfig {
    /// Provider display name used in error messages.
    pub provider_name: String,
    /// API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional organization ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Optional project ID for providers that support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

impl OpenAICompatibleConfig {
    /// Create config for an OpenAI-compatible provider.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "openai-compatible".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: None,
            organization_id: None,
            project_id: None,
        }
    }

    /// Set provider display name used in errors.
    pub fn with_provider_name(mut self, provider_name: impl Into<String>) -> Self {
        self.provider_name = provider_name.into();
        self
    }

    /// Set a custom API base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Set organization ID.
    pub fn with_organization(mut self, organization_id: impl Into<String>) -> Self {
        self.organization_id = Some(organization_id.into());
        self
    }

    /// Set project ID.
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }
}

/// Shared OpenAI-compatible client implementation.
pub struct OpenAICompatible {
    client: Client<AsyncOpenAIConfig>,
    model: String,
    provider_name: String,
    retry_config: RetryConfig,
}

impl OpenAICompatible {
    /// Create a new OpenAI-compatible client.
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, AdkError> {
        let mut openai_config = AsyncOpenAIConfig::new().with_api_key(&config.api_key);

        if let Some(org_id) = &config.organization_id {
            openai_config = openai_config.with_org_id(org_id);
        }

        if let Some(base_url) = &config.base_url {
            openai_config = openai_config.with_api_base(base_url);
        }

        Ok(Self {
            client: Client::with_config(openai_config),
            model: config.model,
            provider_name: config.provider_name,
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
impl Llm for OpenAICompatible {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool, // OpenAI-compatible providers use streaming internally
    ) -> Result<LlmResponseStream, AdkError> {
        let model = self.model.clone();
        let provider_name = self.provider_name.clone();
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();

        let stream = try_stream! {
            // Retries only cover request setup/execution. Stream failures after start are surfaced
            // directly and are not auto-replayed.
            let mut stream = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let model = model.clone();
                let provider_name = provider_name.clone();
                let client = client.clone();
                let request = request_for_retry.clone();
                async move {
                    let messages: Vec<_> = request
                        .contents
                        .iter()
                        .map(convert::content_to_message)
                        .collect();

                    let mut request_builder = CreateChatCompletionRequestArgs::default();
                    request_builder.model(&model).messages(messages);

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
                            request_builder.max_tokens(max_tokens as u32);
                        }

                        if let Some(schema) = &config.response_schema {
                            let mut schema_with_strict = schema.clone();
                            if let Some(obj) = schema_with_strict.as_object_mut() {
                                obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                            }
                            let json_schema = ResponseFormatJsonSchema {
                                name: request.model.replace(['-', '.', '/'], "_"),
                                description: None,
                                schema: Some(schema_with_strict),
                                strict: Some(true),
                            };
                            request_builder.response_format(ResponseFormat::JsonSchema { json_schema });
                        }
                    }

                    let openai_request = request_builder
                        .build()
                        .map_err(|e| AdkError::Model(format!("Failed to build request: {e}")))?;

                    client
                        .chat()
                        .create_stream(openai_request)
                        .await
                        .map_err(|e| {
                            AdkError::Model(format!("{provider_name} API error: {e}"))
                        })
                }
            })
            .await?;

            // For tool calls, we need to accumulate arguments across chunks
            // OpenAI-compatible streams tool call arguments incrementally.
            // Key is index (u32), value is (call_id, name, args_string).
            let mut tool_call_accumulators: std::collections::HashMap<u32, (String, String, String)> =
                std::collections::HashMap::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(chunk) => {
                        if let Some(choice) = chunk.choices.first() {
                            if let Some(tool_calls) = &choice.delta.tool_calls {
                                for tc in tool_calls {
                                    let index = tc.index;

                                    let entry =
                                        tool_call_accumulators.entry(index).or_insert_with(|| {
                                            let call_id = tc
                                                .id
                                                .clone()
                                                .unwrap_or_else(|| format!("call_{index}"));
                                            (call_id, String::new(), String::new())
                                        });

                                    if let Some(id) = &tc.id {
                                        entry.0 = id.clone();
                                    }

                                    if let Some(func) = &tc.function {
                                        if let Some(name) = &func.name {
                                            entry.1 = name.clone();
                                        }

                                        if let Some(args_chunk) = &func.arguments {
                                            entry.2.push_str(args_chunk);
                                        }
                                    }
                                }
                            }

                            if choice.finish_reason.is_some() && !tool_call_accumulators.is_empty() {
                                let mut parts = Vec::new();

                                if let Some(text) = &choice.delta.content {
                                    if !text.is_empty() {
                                        parts.push(Part::Text { text: text.clone() });
                                    }
                                }

                                let mut sorted_calls: Vec<_> = tool_call_accumulators.iter().collect();
                                sorted_calls.sort_by_key(|(idx, _)| *idx);

                                for (_idx, (call_id, name, args_str)) in sorted_calls {
                                    let args: serde_json::Value =
                                        serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
                                    parts.push(Part::FunctionCall {
                                        name: name.clone(),
                                        args,
                                        id: Some(call_id.clone()),
                                    });
                                }

                                let finish_reason = choice.finish_reason.map(|fr| match fr {
                                    async_openai::types::FinishReason::Stop => adk_core::FinishReason::Stop,
                                    async_openai::types::FinishReason::Length => {
                                        adk_core::FinishReason::MaxTokens
                                    }
                                    async_openai::types::FinishReason::ToolCalls => {
                                        adk_core::FinishReason::Stop
                                    }
                                    async_openai::types::FinishReason::ContentFilter => {
                                        adk_core::FinishReason::Safety
                                    }
                                    async_openai::types::FinishReason::FunctionCall => {
                                        adk_core::FinishReason::Stop
                                    }
                                });

                                yield adk_core::LlmResponse {
                                    content: Some(adk_core::Content {
                                        role: "model".to_string(),
                                        parts,
                                    }),
                                    usage_metadata: None,
                                    finish_reason,
                                    citation_metadata: None,
                                    partial: false,
                                    turn_complete: true,
                                    interrupted: false,
                                    error_code: None,
                                    error_message: None,
                                };
                                continue;
                            }
                        }

                        if tool_call_accumulators.is_empty() {
                            let response = convert::from_openai_chunk(&chunk);
                            yield response;
                        } else if let Some(choice) = chunk.choices.first() {
                            if let Some(text) = &choice.delta.content {
                                if !text.is_empty() {
                                    yield adk_core::LlmResponse {
                                        content: Some(adk_core::Content {
                                            role: "model".to_string(),
                                            parts: vec![Part::Text { text: text.clone() }],
                                        }),
                                        usage_metadata: None,
                                        finish_reason: None,
                                        citation_metadata: None,
                                        partial: true,
                                        turn_complete: false,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                    };
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AdkError::Model(format!("Stream error: {e}")))?;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
