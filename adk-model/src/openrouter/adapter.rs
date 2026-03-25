//! Agent-facing `Llm` adapter and extension-bag helpers for OpenRouter.

use super::OPENROUTER_EXTENSION_NAMESPACE;
use super::chat::{
    OpenRouterChatMessageContent, OpenRouterChatRequest, OpenRouterChatResponse,
    OpenRouterChatToolCall, OpenRouterFunctionDescription, OpenRouterImageConfig, OpenRouterPlugin,
    OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterResponseFormat,
    OpenRouterTool, OpenRouterToolChoice,
};
use super::client::OpenRouterClient;
use super::config::OpenRouterApiMode;
use super::convert_chat::{
    adk_contents_to_chat_messages, augment_chat_plugins_for_contents,
    chat_message_reasoning_to_parts,
};
use super::convert_responses::{
    adk_contents_to_response_input, responses_reasoning_items_to_parts,
};
use super::metadata::{
    chat_response_citation_metadata, chat_response_provider_metadata, chat_usage_to_metadata,
    responses_citation_metadata, responses_provider_metadata, responses_usage_to_metadata,
};
use super::responses::{
    OpenRouterResponse, OpenRouterResponseOutputItem, OpenRouterResponseTextConfig,
    OpenRouterResponseTool, OpenRouterResponsesRequest,
};
use crate::retry::{execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, GenerateContentConfig, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Agent-facing OpenRouter options serialized into `GenerateContentConfig::extensions["openrouter"]`.
///
/// These options let callers select the native OpenRouter API mode and pass
/// OpenRouter-specific request parameters through the generic ADK request layer
/// without losing them during serialization.
///
/// # Example
///
/// ```rust
/// use adk_core::GenerateContentConfig;
/// use adk_model::openrouter::{OpenRouterApiMode, OpenRouterRequestOptions};
///
/// let mut config = GenerateContentConfig::default();
/// OpenRouterRequestOptions::default()
///     .with_api_mode(OpenRouterApiMode::Responses)
///     .with_route("fallback")
///     .insert_into_config(&mut config)
///     .expect("options should serialize");
///
/// assert!(config.extensions.contains_key("openrouter"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterRequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_mode: Option<OpenRouterApiMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<OpenRouterPlugin>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_tools: Option<Vec<OpenRouterResponseTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<OpenRouterProviderPreferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenRouterReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<OpenRouterImageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<OpenRouterToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_text: Option<OpenRouterResponseTextConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenRouterResponseFormat>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl OpenRouterRequestOptions {
    /// Set the native OpenRouter API mode used by the adapter.
    #[must_use]
    pub fn with_api_mode(mut self, api_mode: OpenRouterApiMode) -> Self {
        self.api_mode = Some(api_mode);
        self
    }

    /// Set fallback models for OpenRouter routing.
    #[must_use]
    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = Some(models);
        self
    }

    /// Set OpenRouter route selection.
    #[must_use]
    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = Some(route.into());
        self
    }

    /// Append one chat plugin.
    #[must_use]
    pub fn with_plugin(mut self, plugin: OpenRouterPlugin) -> Self {
        self.plugins.get_or_insert_with(Vec::new).push(plugin);
        self
    }

    /// Append one Responses API server tool.
    #[must_use]
    pub fn with_response_tool(mut self, tool: OpenRouterResponseTool) -> Self {
        self.response_tools.get_or_insert_with(Vec::new).push(tool);
        self
    }

    /// Set OpenRouter provider preferences.
    #[must_use]
    pub fn with_provider_preferences(mut self, provider: OpenRouterProviderPreferences) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set OpenRouter reasoning configuration.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: OpenRouterReasoningConfig) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// Set a previous response ID for Responses API chaining.
    #[must_use]
    pub fn with_previous_response_id(mut self, previous_response_id: impl Into<String>) -> Self {
        self.previous_response_id = Some(previous_response_id.into());
        self
    }

    /// Set a prompt cache key for the Responses API.
    #[must_use]
    pub fn with_prompt_cache_key(mut self, prompt_cache_key: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(prompt_cache_key.into());
        self
    }

    /// Set requested response modalities.
    #[must_use]
    pub fn with_modalities(mut self, modalities: Vec<String>) -> Self {
        self.modalities = Some(modalities);
        self
    }

    /// Set Responses API include fields.
    #[must_use]
    pub fn with_include(mut self, include: Vec<String>) -> Self {
        self.include = Some(include);
        self
    }

    /// Serialize the options into a JSON extension value.
    pub fn to_extension_value(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.request_options_serialize_failed",
                "Failed to serialize OpenRouter request options",
            )
            .with_provider("openrouter")
            .with_source(err)
        })
    }

    /// Deserialize the options from a JSON extension value.
    pub fn from_extension_value(value: &Value) -> Result<Self> {
        serde_json::from_value(value.clone()).map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openrouter.invalid_request_options",
                "OpenRouter extension options are not valid JSON for OpenRouterRequestOptions",
            )
            .with_provider("openrouter")
            .with_source(err)
        })
    }

    /// Extract OpenRouter request options from `GenerateContentConfig`.
    pub fn from_generate_config(config: Option<&GenerateContentConfig>) -> Result<Option<Self>> {
        config
            .and_then(|config| config.extensions.get(OPENROUTER_EXTENSION_NAMESPACE))
            .map(Self::from_extension_value)
            .transpose()
    }

    /// Insert the serialized options into `GenerateContentConfig::extensions`.
    pub fn insert_into_config(&self, config: &mut GenerateContentConfig) -> Result<()> {
        config
            .extensions
            .insert(OPENROUTER_EXTENSION_NAMESPACE.to_string(), self.to_extension_value()?);
        Ok(())
    }
}

#[async_trait]
impl Llm for OpenRouterClient {
    fn name(&self) -> &str {
        self.model()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        let request_options =
            OpenRouterRequestOptions::from_generate_config(request.config.as_ref())?
                .unwrap_or_default();
        let model_name = effective_model_name(self, &request);
        let api_mode = request_options.api_mode.unwrap_or(self.config().default_api_mode);
        let usage_span = adk_telemetry::llm_generate_span("openrouter", &model_name, stream);

        let response_stream = match api_mode {
            OpenRouterApiMode::ChatCompletions => {
                build_chat_llm_stream(self, request, &request_options, stream).await?
            }
            OpenRouterApiMode::Responses => {
                build_responses_llm_stream(self, request, &request_options, stream).await?
            }
        };

        Ok(crate::usage_tracking::with_usage_tracking(response_stream, usage_span))
    }
}

async fn build_chat_llm_stream(
    client: &OpenRouterClient,
    request: LlmRequest,
    request_options: &OpenRouterRequestOptions,
    stream: bool,
) -> Result<LlmResponseStream> {
    let native_request = build_chat_request(client, &request, request_options, stream)?;

    if stream {
        let native_request_for_retry = native_request.clone();
        let native_stream =
            execute_with_retry(client.retry_config(), is_retryable_model_error, || {
                let native_request = native_request_for_retry.clone();
                async move { client.send_chat_stream(native_request).await }
            })
            .await?;

        let response_stream = try_stream! {
            let mut native_stream = native_stream;
            let mut state = ChatStreamState::default();
            let mut final_parts = Vec::new();
            let mut final_finish_reason = None;
            let mut final_usage_metadata = None;

            while let Some(item) = native_stream.next().await {
                match item? {
                    super::stream::OpenRouterChatStreamItem::Chunk(chunk) => {
                        if let Some(usage_metadata) = chunk.usage.as_ref().map(chat_usage_to_metadata) {
                            final_usage_metadata = Some(usage_metadata);
                        }

                        if let Some(choice) = chunk.choices.first() {
                            if let Some(delta) = choice.delta.as_ref() {
                                let (reasoning_parts, reasoning_metadata) =
                                    chat_message_reasoning_to_parts(delta);
                                if let Some(response) = llm_stream_response(
                                    reasoning_parts,
                                    None,
                                    None,
                                    reasoning_metadata,
                                    true,
                                    false,
                                ) {
                                    yield response;
                                }

                                let text_parts = chat_content_to_parts(delta.content.as_ref());
                                if let Some(response) = llm_stream_response(
                                    text_parts,
                                    None,
                                    None,
                                    None,
                                    true,
                                    false,
                                ) {
                                    yield response;
                                }

                                if let Some(tool_calls) = delta.tool_calls.as_ref() {
                                    state.capture_chat_tool_calls(tool_calls);
                                }
                            }

                            if let Some(finish_reason) = choice
                                .finish_reason
                                .as_deref()
                                .map(chat_finish_reason_from_str)
                            {
                                final_parts.extend(state.drain_chat_tool_calls());
                                final_finish_reason = Some(finish_reason);
                            }
                        }
                    }
                    super::stream::OpenRouterChatStreamItem::Done => {
                        if let Some(response) = llm_stream_response(
                            std::mem::take(&mut final_parts),
                            final_usage_metadata.take(),
                            final_finish_reason.take(),
                            None,
                            false,
                            true,
                        ) {
                            yield response;
                        }
                    }
                    super::stream::OpenRouterChatStreamItem::Error(_) => unreachable!(),
                }
            }

            if let Some(response) = llm_stream_response(
                final_parts,
                final_usage_metadata,
                final_finish_reason,
                None,
                false,
                true,
            ) {
                yield response;
            }
        };

        Ok(Box::pin(response_stream))
    } else {
        let native_request_for_retry = native_request.clone();
        let response = execute_with_retry(client.retry_config(), is_retryable_model_error, || {
            let native_request = native_request_for_retry.clone();
            async move { client.send_chat(native_request).await }
        })
        .await?;
        let mapped = chat_response_to_llm_response(&response);
        let response_stream = try_stream! {
            yield mapped;
        };

        Ok(Box::pin(response_stream))
    }
}

async fn build_responses_llm_stream(
    client: &OpenRouterClient,
    request: LlmRequest,
    request_options: &OpenRouterRequestOptions,
    stream: bool,
) -> Result<LlmResponseStream> {
    let native_request = build_responses_request(client, &request, request_options, stream)?;

    if stream {
        let native_request_for_retry = native_request.clone();
        let native_stream =
            execute_with_retry(client.retry_config(), is_retryable_model_error, || {
                let native_request = native_request_for_retry.clone();
                async move { client.create_response_stream(native_request).await }
            })
            .await?;

        let response_stream = try_stream! {
            let mut native_stream = native_stream;
            let mut state = ResponsesStreamState::default();

            while let Some(item) = native_stream.next().await {
                match item? {
                    super::stream::OpenRouterResponsesStreamItem::Event(event) => {
                        match event.kind.as_str() {
                            "response.output_item.added" | "response.output_item.done" => {
                                if let Some(item) = event.item.as_ref() {
                                    state.capture_function_call_item(item, event.item_id.as_deref());
                                }
                            }
                            "response.output_text.delta" => {
                                if let Some(delta) = event.delta.as_deref() {
                                    if let Some(response) = llm_stream_response(
                                        vec![Part::Text { text: delta.to_string() }],
                                        None,
                                        None,
                                        None,
                                        true,
                                        false,
                                    ) {
                                        yield response;
                                    }
                                }
                            }
                            "response.reasoning.delta"
                            | "response.reasoning_text.delta"
                            | "response.reasoning_summary_text.delta" => {
                                if let Some(delta) = event.delta.as_deref().or(event.text.as_deref()) {
                                    if let Some(response) = llm_stream_response(
                                        vec![Part::Thinking {
                                            thinking: delta.to_string(),
                                            signature: None,
                                        }],
                                        None,
                                        None,
                                        None,
                                        true,
                                        false,
                                    ) {
                                        yield response;
                                    }
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                if let (Some(item_id), Some(delta)) =
                                    (event.item_id.as_deref(), event.delta.as_deref())
                                {
                                    state.append_function_call_arguments(item_id, delta);
                                }
                            }
                            "response.function_call_arguments.done" => {
                                if let Some(item_id) = event.item_id.as_deref() {
                                    if let Some(part) =
                                        state.complete_function_call(item_id, event.arguments.as_deref())
                                    {
                                        if let Some(response) = llm_stream_response(
                                            vec![part],
                                            None,
                                            Some(FinishReason::Stop),
                                            None,
                                            false,
                                            true,
                                        ) {
                                            yield response;
                                        }
                                    }
                                }
                            }
                            "response.completed" => {
                                if let Some(response) = event.response.as_ref() {
                                    if let Some(mapped) = responses_response_to_llm_response(
                                        response,
                                        Some(&state.emitted_function_call_ids),
                                    ) {
                                        yield mapped;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    super::stream::OpenRouterResponsesStreamItem::Done => {}
                    super::stream::OpenRouterResponsesStreamItem::Error(_) => unreachable!(),
                }
            }
        };

        Ok(Box::pin(response_stream))
    } else {
        let native_request_for_retry = native_request.clone();
        let response = execute_with_retry(client.retry_config(), is_retryable_model_error, || {
            let native_request = native_request_for_retry.clone();
            async move { client.create_response(native_request).await }
        })
        .await?;
        let mapped = responses_response_to_llm_response(&response, None);
        let response_stream = try_stream! {
            if let Some(mapped) = mapped {
                yield mapped;
            }
        };

        Ok(Box::pin(response_stream))
    }
}

fn build_chat_request(
    client: &OpenRouterClient,
    request: &LlmRequest,
    request_options: &OpenRouterRequestOptions,
    stream: bool,
) -> Result<OpenRouterChatRequest> {
    let mut chat_request = OpenRouterChatRequest {
        model: effective_model_name(client, request),
        messages: adk_contents_to_chat_messages(&request.contents)?,
        tools: chat_tools_from_llm_request(&request.tools),
        tool_choice: request_options.tool_choice.clone(),
        parallel_tool_calls: request_options.parallel_tool_calls,
        plugins: chat_plugins_for_request(&request.contents, request_options),
        provider: request_options.provider.clone(),
        models: request_options.models.clone(),
        route: request_options.route.clone(),
        reasoning: request_options.reasoning.clone(),
        reasoning_content: request_options.reasoning_content.clone(),
        reasoning_details: request_options.reasoning_details.clone(),
        response_format: request_options.response_format.clone(),
        modalities: request_options.modalities.clone(),
        image_config: request_options.image_config.clone(),
        metadata: request_options.metadata.clone(),
        session_id: request_options.session_id.clone(),
        trace: request_options.trace.clone(),
        stream: stream.then_some(true),
        stream_options: stream.then_some(json!({ "include_usage": true })),
        user: request_options.user.clone(),
        extra: request_options.extra.clone(),
        ..Default::default()
    };

    if let Some(config) = request.config.as_ref() {
        apply_generate_config_to_chat_request(&mut chat_request, config, &request.model);
    }

    Ok(chat_request)
}

fn build_responses_request(
    client: &OpenRouterClient,
    request: &LlmRequest,
    request_options: &OpenRouterRequestOptions,
    stream: bool,
) -> Result<OpenRouterResponsesRequest> {
    let mut responses_request = OpenRouterResponsesRequest {
        input: Some(adk_contents_to_response_input(&request.contents)?),
        instructions: request_options.instructions.clone(),
        metadata: request_options.metadata.clone(),
        tools: responses_tools_from_llm_request(&request.tools, request_options),
        tool_choice: request_options.tool_choice.clone(),
        parallel_tool_calls: request_options.parallel_tool_calls,
        model: Some(effective_model_name(client, request)),
        models: request_options.models.clone(),
        text: request_options.response_text.clone(),
        reasoning: request_options.reasoning.clone(),
        reasoning_content: request_options.reasoning_content.clone(),
        reasoning_details: request_options.reasoning_details.clone(),
        image_config: request_options.image_config.clone(),
        modalities: request_options.modalities.clone(),
        prompt_cache_key: request_options.prompt_cache_key.clone(),
        previous_response_id: request_options.previous_response_id.clone(),
        include: request_options.include.clone(),
        stream: stream.then_some(true),
        provider: request_options.provider.clone(),
        route: request_options.route.clone(),
        session_id: request_options.session_id.clone(),
        trace: request_options.trace.clone(),
        user: request_options.user.clone(),
        plugins: request_options.plugins.clone(),
        extra: request_options.extra.clone(),
        ..Default::default()
    };

    if let Some(config) = request.config.as_ref() {
        apply_generate_config_to_responses_request(&mut responses_request, config, &request.model);
    }

    Ok(responses_request)
}

fn effective_model_name(client: &OpenRouterClient, request: &LlmRequest) -> String {
    if request.model.trim().is_empty() { client.model().to_string() } else { request.model.clone() }
}

fn chat_tools_from_llm_request(tools: &HashMap<String, Value>) -> Option<Vec<OpenRouterTool>> {
    (!tools.is_empty()).then_some(
        tools
            .iter()
            .map(|(name, declaration)| OpenRouterTool {
                kind: "function".to_string(),
                function: Some(OpenRouterFunctionDescription {
                    name: name.clone(),
                    description: declaration
                        .get("description")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    parameters: declaration.get("parameters").cloned(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    )
}

fn responses_tools_from_llm_request(
    tools: &HashMap<String, Value>,
    request_options: &OpenRouterRequestOptions,
) -> Option<Vec<OpenRouterResponseTool>> {
    let mut response_tools = tools
        .iter()
        .map(|(name, declaration)| OpenRouterResponseTool {
            kind: "function".to_string(),
            name: Some(name.clone()),
            description: declaration
                .get("description")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            parameters: declaration.get("parameters").cloned(),
            strict: declaration.get("strict").and_then(Value::as_bool),
            ..Default::default()
        })
        .collect::<Vec<_>>();

    if let Some(extra_tools) = request_options.response_tools.as_ref() {
        response_tools.extend(extra_tools.clone());
    }

    (!response_tools.is_empty()).then_some(response_tools)
}

fn chat_plugins_for_request(
    contents: &[Content],
    request_options: &OpenRouterRequestOptions,
) -> Option<Vec<OpenRouterPlugin>> {
    let plugins = augment_chat_plugins_for_contents(
        contents,
        request_options.plugins.clone().unwrap_or_default(),
    );
    (!plugins.is_empty()).then_some(plugins)
}

fn apply_generate_config_to_chat_request(
    request: &mut OpenRouterChatRequest,
    config: &GenerateContentConfig,
    model_name: &str,
) {
    request.temperature = config.temperature;
    request.top_p = config.top_p;
    request.max_completion_tokens = config.max_output_tokens;
    request.frequency_penalty = config.frequency_penalty;
    request.presence_penalty = config.presence_penalty;
    request.seed = config.seed;
    request.top_logprobs = config.top_logprobs;

    if !config.stop_sequences.is_empty() {
        request.stop =
            Some(Value::Array(config.stop_sequences.iter().cloned().map(Value::String).collect()));
    }

    if request.response_format.is_none() {
        request.response_format =
            config.response_schema.as_ref().map(|schema| OpenRouterResponseFormat {
                kind: "json_schema".to_string(),
                json_schema: Some(json!({
                    "name": sanitized_schema_name(model_name),
                    "schema": schema,
                    "strict": true
                })),
                ..Default::default()
            });
    }
}

fn apply_generate_config_to_responses_request(
    request: &mut OpenRouterResponsesRequest,
    config: &GenerateContentConfig,
    model_name: &str,
) {
    request.temperature = config.temperature;
    request.top_p = config.top_p;
    request.top_k = config.top_k;
    request.max_output_tokens = config.max_output_tokens;
    request.frequency_penalty = config.frequency_penalty;
    request.presence_penalty = config.presence_penalty;
    request.top_logprobs = config.top_logprobs;

    if !config.stop_sequences.is_empty() {
        request.extra.insert(
            "stop".to_string(),
            Value::Array(config.stop_sequences.iter().cloned().map(Value::String).collect()),
        );
    }

    if let Some(schema) = config.response_schema.as_ref() {
        let mut text_config = request.text.clone().unwrap_or_default();
        if text_config.format.is_none() {
            text_config.format = Some(json!({
                "type": "json_schema",
                "name": sanitized_schema_name(model_name),
                "schema": schema,
                "strict": true
            }));
        }
        request.text = Some(text_config);
    }
}

fn sanitized_schema_name(model_name: &str) -> String {
    model_name.replace(['-', '.', '/'], "_")
}

fn chat_response_to_llm_response(response: &OpenRouterChatResponse) -> LlmResponse {
    let mut parts = Vec::new();
    let mut provider_metadata = Map::new();

    if let Some(choice) = response.choices.first() {
        if let Some(message) = choice.message.as_ref() {
            let (reasoning_parts, reasoning_metadata) = chat_message_reasoning_to_parts(message);
            parts.extend(reasoning_parts);
            parts.extend(chat_content_to_parts(message.content.as_ref()));
            parts.extend(chat_tool_calls_to_parts(message.tool_calls.as_ref()));
            merge_metadata_object(&mut provider_metadata, reasoning_metadata);
        }

        if let Some(logprobs) = choice.logprobs.clone() {
            provider_metadata.insert("logprobs".to_string(), logprobs);
        }
        if !choice.extra.is_empty() {
            provider_metadata.insert("choice_extra".to_string(), json!(choice.extra));
        }
    }

    merge_metadata_object(&mut provider_metadata, chat_response_provider_metadata(response));

    if let Some(system_fingerprint) = response.system_fingerprint.as_ref() {
        provider_metadata
            .insert("system_fingerprint".to_string(), Value::String(system_fingerprint.clone()));
    }
    if !response.extra.is_empty() {
        provider_metadata.insert("response_extra".to_string(), json!(response.extra));
    }

    LlmResponse {
        content: (!parts.is_empty()).then_some(Content { role: "model".to_string(), parts }),
        usage_metadata: response.usage.as_ref().map(chat_usage_to_metadata),
        finish_reason: response
            .choices
            .first()
            .and_then(|choice| choice.finish_reason.as_deref())
            .map(chat_finish_reason_from_str),
        citation_metadata: chat_response_citation_metadata(response),
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: (!provider_metadata.is_empty())
            .then_some(Value::Object(provider_metadata)),
    }
}

fn responses_response_to_llm_response(
    response: &OpenRouterResponse,
    emitted_function_call_ids: Option<&HashSet<String>>,
) -> Option<LlmResponse> {
    let mut parts = Vec::new();
    let mut provider_metadata = Map::new();
    let empty_emitted = HashSet::new();
    let emitted_function_call_ids = emitted_function_call_ids.unwrap_or(&empty_emitted);

    let (reasoning_parts, reasoning_metadata) =
        responses_reasoning_items_to_parts(&response.output);
    parts.extend(reasoning_parts);
    merge_metadata_object(&mut provider_metadata, reasoning_metadata);

    for item in &response.output {
        match item.kind.as_str() {
            "message" => parts.extend(response_message_item_to_parts(item)),
            "function_call" => {
                if let Some(part) = response_function_call_part(item) {
                    let part_id = match &part {
                        Part::FunctionCall { id, .. } => id.clone(),
                        _ => None,
                    };
                    if part_id
                        .as_ref()
                        .is_none_or(|part_id| !emitted_function_call_ids.contains(part_id))
                    {
                        parts.push(part);
                    }
                }
            }
            _ => {}
        }
    }

    merge_metadata_object(&mut provider_metadata, responses_provider_metadata(response));

    if let Some(previous_response_id) = response.previous_response_id.as_ref() {
        provider_metadata.insert(
            "previous_response_id".to_string(),
            Value::String(previous_response_id.clone()),
        );
    }
    if let Some(incomplete_details) = response.incomplete_details.clone() {
        provider_metadata.insert("incomplete_details".to_string(), incomplete_details);
    }
    if let Some(error) = response.error.clone() {
        provider_metadata.insert("error".to_string(), error);
    }
    if !response.extra.is_empty() {
        provider_metadata.insert("response_extra".to_string(), json!(response.extra));
    }

    let usage_metadata = response.usage.as_ref().map(responses_usage_to_metadata);
    let finish_reason = responses_finish_reason(response);
    let citation_metadata = responses_citation_metadata(response);
    let content = (!parts.is_empty()).then_some(Content { role: "model".to_string(), parts });

    if content.is_none()
        && usage_metadata.is_none()
        && finish_reason.is_none()
        && citation_metadata.is_none()
        && provider_metadata.is_empty()
    {
        return None;
    }

    Some(LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: (!provider_metadata.is_empty())
            .then_some(Value::Object(provider_metadata)),
    })
}

fn chat_content_to_parts(content: Option<&OpenRouterChatMessageContent>) -> Vec<Part> {
    match content {
        Some(OpenRouterChatMessageContent::Text(text)) if !text.is_empty() => {
            vec![Part::Text { text: text.clone() }]
        }
        Some(OpenRouterChatMessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|part| {
                (part.kind == "text")
                    .then_some(part.text.as_ref())
                    .flatten()
                    .filter(|text| !text.is_empty())
                    .map(|text| Part::Text { text: text.clone() })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn chat_tool_calls_to_parts(tool_calls: Option<&Vec<OpenRouterChatToolCall>>) -> Vec<Part> {
    tool_calls.into_iter().flatten().filter_map(chat_tool_call_to_part).collect()
}

fn chat_tool_call_to_part(tool_call: &OpenRouterChatToolCall) -> Option<Part> {
    let function = tool_call.function.as_ref()?;
    let name = function.name.clone()?;
    let arguments = function.arguments.as_deref()?;
    let args = serde_json::from_str(arguments).ok()?;

    Some(Part::FunctionCall { name, args, id: tool_call.id.clone(), thought_signature: None })
}

fn response_message_item_to_parts(item: &OpenRouterResponseOutputItem) -> Vec<Part> {
    item.content
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                return part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(|text| Part::Text { text: text.to_string() });
            }
            None
        })
        .collect()
}

fn response_function_call_part(item: &OpenRouterResponseOutputItem) -> Option<Part> {
    if item.kind != "function_call" {
        return None;
    }

    let name = item.name.clone()?;
    let arguments = item.arguments.as_deref()?;
    let args = serde_json::from_str(arguments).ok()?;

    Some(Part::FunctionCall {
        name,
        args,
        id: item.call_id.clone().or_else(|| item.id.clone()),
        thought_signature: None,
    })
}

fn chat_finish_reason_from_str(finish_reason: &str) -> FinishReason {
    match finish_reason {
        "stop" | "tool_calls" | "function_call" => FinishReason::Stop,
        "length" | "max_tokens" => FinishReason::MaxTokens,
        "content_filter" | "safety" => FinishReason::Safety,
        "recitation" => FinishReason::Recitation,
        _ => FinishReason::Other,
    }
}

fn responses_finish_reason(response: &OpenRouterResponse) -> Option<FinishReason> {
    match response.status.as_deref() {
        Some("completed") => Some(FinishReason::Stop),
        Some("incomplete") => match response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => Some(FinishReason::MaxTokens),
            Some("content_filter") => Some(FinishReason::Safety),
            Some(_) | None => Some(FinishReason::Other),
        },
        Some("failed" | "cancelled") => Some(FinishReason::Other),
        _ => None,
    }
}

fn merge_metadata_object(target: &mut Map<String, Value>, value: Option<Value>) {
    if let Some(Value::Object(object)) = value {
        target.extend(object);
    }
}

fn llm_stream_response(
    parts: Vec<Part>,
    usage_metadata: Option<adk_core::UsageMetadata>,
    finish_reason: Option<FinishReason>,
    provider_metadata: Option<Value>,
    partial: bool,
    turn_complete: bool,
) -> Option<LlmResponse> {
    if parts.is_empty()
        && usage_metadata.is_none()
        && finish_reason.is_none()
        && provider_metadata.is_none()
    {
        return None;
    }

    Some(LlmResponse {
        content: (!parts.is_empty()).then_some(Content { role: "model".to_string(), parts }),
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial,
        turn_complete,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata,
    })
}

#[derive(Debug, Default)]
struct ChatStreamState {
    pending_tool_calls: BTreeMap<usize, PendingChatToolCall>,
}

#[derive(Debug, Default)]
struct PendingChatToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ChatStreamState {
    fn capture_chat_tool_calls(&mut self, tool_calls: &[OpenRouterChatToolCall]) {
        for (index, tool_call) in tool_calls.iter().enumerate() {
            let pending = self.pending_tool_calls.entry(index).or_default();
            if let Some(id) = tool_call.id.as_ref() {
                pending.id = Some(id.clone());
            }
            if let Some(function) = tool_call.function.as_ref() {
                if let Some(name) = function.name.as_ref() {
                    pending.name = Some(name.clone());
                }
                if let Some(arguments) = function.arguments.as_ref() {
                    pending.arguments.push_str(arguments);
                }
            }
        }
    }

    fn drain_chat_tool_calls(&mut self) -> Vec<Part> {
        self.pending_tool_calls
            .iter()
            .filter_map(|(_, pending)| {
                let name = pending.name.clone()?;
                let args = serde_json::from_str(&pending.arguments).ok()?;
                Some(Part::FunctionCall {
                    name,
                    args,
                    id: pending.id.clone(),
                    thought_signature: None,
                })
            })
            .collect::<Vec<_>>()
            .tap(|_| self.pending_tool_calls.clear())
    }
}

#[derive(Debug, Default)]
struct ResponsesStreamState {
    pending_function_calls: HashMap<String, PendingResponseFunctionCall>,
    emitted_function_call_ids: HashSet<String>,
}

#[derive(Debug, Default)]
struct PendingResponseFunctionCall {
    id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ResponsesStreamState {
    fn capture_function_call_item(
        &mut self,
        item: &OpenRouterResponseOutputItem,
        event_item_id: Option<&str>,
    ) {
        if item.kind != "function_call" {
            return;
        }

        let key = event_item_id
            .map(ToString::to_string)
            .or_else(|| item.id.clone())
            .or_else(|| item.call_id.clone());

        if let Some(key) = key {
            let pending = self.pending_function_calls.entry(key).or_default();
            pending.id = item.id.clone();
            pending.call_id = item.call_id.clone();
            pending.name = item.name.clone();
            if let Some(arguments) = item.arguments.as_ref() {
                pending.arguments = arguments.clone();
            }
        }
    }

    fn append_function_call_arguments(&mut self, item_id: &str, delta: &str) {
        self.pending_function_calls
            .entry(item_id.to_string())
            .or_default()
            .arguments
            .push_str(delta);
    }

    fn complete_function_call(
        &mut self,
        item_id: &str,
        final_arguments: Option<&str>,
    ) -> Option<Part> {
        let pending = self.pending_function_calls.entry(item_id.to_string()).or_default();
        if let Some(final_arguments) = final_arguments {
            pending.arguments = final_arguments.to_string();
        }

        let name = pending.name.clone()?;
        let args = serde_json::from_str(&pending.arguments).ok()?;
        let id = pending.call_id.clone().or_else(|| pending.id.clone());

        if let Some(identifier) = id.as_ref() {
            self.emitted_function_call_ids.insert(identifier.clone());
        } else {
            self.emitted_function_call_ids.insert(item_id.to_string());
        }

        Some(Part::FunctionCall { name, args, id, thought_signature: None })
    }
}

trait Tap: Sized {
    fn tap<F: FnOnce(&Self)>(self, function: F) -> Self {
        function(&self);
        self
    }
}

impl<T> Tap for T {}

#[cfg(test)]
mod tests {
    use super::OpenRouterRequestOptions;
    use crate::openrouter::chat::{
        OpenRouterPlugin, OpenRouterProviderPreferences, OpenRouterReasoningConfig,
    };
    use crate::openrouter::responses::OpenRouterResponseTool;
    use crate::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
    use adk_core::{GenerateContentConfig, Llm, LlmRequest, Part};
    use futures::StreamExt;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn request_options_round_trip_through_extension_bag() {
        let options = OpenRouterRequestOptions::default()
            .with_api_mode(OpenRouterApiMode::Responses)
            .with_models(vec!["openai/gpt-5.2".to_string(), "openai/gpt-5-mini".to_string()])
            .with_route("fallback")
            .with_plugin(OpenRouterPlugin {
                id: "web".to_string(),
                enabled: Some(true),
                ..Default::default()
            })
            .with_response_tool(OpenRouterResponseTool {
                kind: "web_search".to_string(),
                ..Default::default()
            })
            .with_provider_preferences(OpenRouterProviderPreferences {
                zdr: Some(true),
                ..Default::default()
            })
            .with_reasoning(OpenRouterReasoningConfig {
                effort: Some("high".to_string()),
                ..Default::default()
            })
            .with_previous_response_id("resp_previous")
            .with_prompt_cache_key("cache-key")
            .with_modalities(vec!["text".to_string(), "image".to_string()])
            .with_include(vec!["reasoning.encrypted_content".to_string()]);

        let mut config = GenerateContentConfig::default();
        options.insert_into_config(&mut config).expect("options should insert into config");

        let request = LlmRequest::new("openai/gpt-5.2", vec![]).with_config(config);
        let round_trip_request: LlmRequest = serde_json::from_value(
            serde_json::to_value(&request).expect("request should serialize"),
        )
        .expect("request should deserialize");
        let round_trip =
            OpenRouterRequestOptions::from_generate_config(round_trip_request.config.as_ref())
                .expect("options should parse")
                .expect("openrouter options should exist");

        assert_eq!(round_trip, options);
    }

    #[tokio::test]
    async fn generate_content_chat_mode_maps_reasoning_and_function_calls() {
        let server = MockServer::start().await;
        let expected_body = json!({
            "model": "openai/gpt-5.2",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "plugins": [
                {
                    "id": "web",
                    "enabled": true
                }
            ],
            "provider": {
                "zdr": true
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_json(&expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "model": "openai/gpt-5.2",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "reasoning": "Need to call the weather tool.",
                            "content": "",
                            "tool_calls": [
                                {
                                    "id": "call_weather",
                                    "type": "function",
                                    "function": {
                                        "name": "lookup_weather",
                                        "arguments": "{\"city\":\"Nairobi\"}"
                                    }
                                }
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut config = GenerateContentConfig::default();
        OpenRouterRequestOptions::default()
            .with_plugin(OpenRouterPlugin {
                id: "web".to_string(),
                enabled: Some(true),
                ..Default::default()
            })
            .with_provider_preferences(OpenRouterProviderPreferences {
                zdr: Some(true),
                ..Default::default()
            })
            .insert_into_config(&mut config)
            .expect("options should insert");

        let mut stream = client
            .generate_content(
                LlmRequest::new(
                    "openai/gpt-5.2",
                    vec![adk_core::Content::new("user").with_text("hello")],
                )
                .with_config(config),
                false,
            )
            .await
            .expect("generation should start");

        let response = stream.next().await.expect("response").expect("response should succeed");
        let content = response.content.expect("content should exist");

        assert_eq!(response.finish_reason, Some(adk_core::FinishReason::Stop));
        assert!(
            matches!(&content.parts[0], Part::Thinking { thinking, .. } if thinking == "Need to call the weather tool.")
        );
        assert!(
            matches!(&content.parts[1], Part::FunctionCall { name, id, .. } if name == "lookup_weather" && id.as_deref() == Some("call_weather"))
        );
    }

    #[tokio::test]
    async fn generate_content_chat_stream_emits_one_final_chunk_when_usage_arrives_after_finish() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"openai/gpt-5.2\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"}}]}\n\n\
                 data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"openai/gpt-5.2\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                 data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"openai/gpt-5.2\",\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n\
                 data: [DONE]\n\n",
            ))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut stream = client
            .generate_content(
                LlmRequest::new(
                    "openai/gpt-5.2",
                    vec![adk_core::Content::new("user").with_text("hello")],
                ),
                true,
            )
            .await
            .expect("generation should start");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("chunk should succeed"));
        }

        assert_eq!(responses.len(), 2);
        assert!(responses[0].partial);
        assert!(!responses[0].turn_complete);
        assert_eq!(
            responses[0]
                .content
                .as_ref()
                .and_then(|content| content.parts.first())
                .and_then(Part::text),
            Some("hello")
        );

        assert!(!responses[1].partial);
        assert!(responses[1].turn_complete);
        assert_eq!(responses[1].finish_reason, Some(adk_core::FinishReason::Stop));
        assert_eq!(
            responses[1].usage_metadata.as_ref().map(|usage| usage.total_token_count),
            Some(6)
        );
    }

    #[tokio::test]
    async fn generate_content_responses_mode_uses_extension_options_after_request_round_trip() {
        let server = MockServer::start().await;
        let expected_body = json!({
            "model": "openai/gpt-5.2",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": "hello"
                }
            ],
            "tools": [
                {
                    "type": "web_search"
                }
            ],
            "reasoning": {
                "effort": "medium"
            },
            "previous_response_id": "resp_previous",
            "prompt_cache_key": "cache-key",
            "include": ["reasoning.encrypted_content"],
            "route": "fallback",
            "models": ["openai/gpt-5.2", "openai/gpt-5-mini"]
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_json(&expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp-1",
                "model": "openai/gpt-5.2",
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "id": "msg-1",
                        "role": "assistant",
                        "status": "completed",
                        "content": [
                            {
                                "type": "output_text",
                                "text": "hello back",
                                "annotations": []
                            }
                        ]
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": { "cached_tokens": 1 },
                    "output_tokens": 6,
                    "output_tokens_details": { "reasoning_tokens": 2 },
                    "total_tokens": 16,
                    "cost": 0.001
                }
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut config = GenerateContentConfig::default();
        OpenRouterRequestOptions::default()
            .with_api_mode(OpenRouterApiMode::Responses)
            .with_response_tool(OpenRouterResponseTool {
                kind: "web_search".to_string(),
                ..Default::default()
            })
            .with_reasoning(OpenRouterReasoningConfig {
                effort: Some("medium".to_string()),
                ..Default::default()
            })
            .with_previous_response_id("resp_previous")
            .with_prompt_cache_key("cache-key")
            .with_include(vec!["reasoning.encrypted_content".to_string()])
            .with_route("fallback")
            .with_models(vec!["openai/gpt-5.2".to_string(), "openai/gpt-5-mini".to_string()])
            .insert_into_config(&mut config)
            .expect("options should insert");

        let request = LlmRequest::new(
            "openai/gpt-5.2",
            vec![adk_core::Content::new("user").with_text("hello")],
        )
        .with_config(config);
        let round_trip_request: LlmRequest = serde_json::from_value(
            serde_json::to_value(&request).expect("request should serialize"),
        )
        .expect("request should deserialize");

        let mut stream = client
            .generate_content(round_trip_request, false)
            .await
            .expect("generation should start");

        let response = stream.next().await.expect("response").expect("response should succeed");
        assert_eq!(
            response
                .content
                .as_ref()
                .and_then(|content| content.parts.first())
                .and_then(Part::text),
            Some("hello back")
        );
        assert_eq!(response.usage_metadata.as_ref().and_then(|usage| usage.cost), Some(0.001));
    }

    #[tokio::test]
    async fn generate_content_responses_stream_emits_complete_function_call_once_arguments_finish()
    {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_weather\",\"name\":\"lookup_weather\",\"arguments\":\"\",\"status\":\"in_progress\"},\"sequence_number\":1}\n\n\
                 data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\":\\\"Na\",\"sequence_number\":2}\n\n\
                 data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"irobi\\\"}\",\"sequence_number\":3}\n\n\
                 data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"sequence_number\":4}\n\n\
                 data: [DONE]\n\n",
            ))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut config = GenerateContentConfig::default();
        OpenRouterRequestOptions::default()
            .with_api_mode(OpenRouterApiMode::Responses)
            .insert_into_config(&mut config)
            .expect("options should insert");

        let mut stream = client
            .generate_content(
                LlmRequest::new(
                    "openai/gpt-5.2",
                    vec![adk_core::Content::new("user").with_text("weather")],
                )
                .with_config(config),
                true,
            )
            .await
            .expect("generation should start");

        let response = stream.next().await.expect("response").expect("response should succeed");
        let part = response
            .content
            .as_ref()
            .and_then(|content| content.parts.first())
            .expect("function call part should exist");

        match part {
            Part::FunctionCall { name, args, id, .. } => {
                assert_eq!(name, "lookup_weather");
                assert_eq!(id.as_deref(), Some("call_weather"));
                assert_eq!(args["city"], json!("Nairobi"));
            }
            other => panic!("expected function call part, got {other:?}"),
        }

        assert!(response.turn_complete);
        assert!(!response.partial);
        assert!(stream.next().await.is_none());
    }
}
