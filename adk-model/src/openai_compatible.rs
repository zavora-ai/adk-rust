//! Shared OpenAI-compatible provider implementation.

use crate::openai::convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, UsageMetadata,
};
use async_openai::types::chat::{
    CreateChatCompletionRequestArgs, ReasoningEffort, ResponseFormat, ResponseFormatJsonSchema,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Optional reasoning effort for OpenAI reasoning models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
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
            reasoning_effort: None,
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

    /// Set reasoning effort for reasoning models.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    // ── Provider presets ─────────────────────────────────────────

    /// Fireworks AI preset.
    ///
    /// Default model: `accounts/fireworks/models/llama-v3p1-8b-instruct`
    pub fn fireworks(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("fireworks")
            .with_base_url("https://api.fireworks.ai/inference/v1")
    }

    /// Together AI preset.
    ///
    /// Default model: `meta-llama/Llama-3.3-70B-Instruct-Turbo`
    pub fn together(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("together")
            .with_base_url("https://api.together.xyz/v1")
    }

    /// Mistral AI preset.
    ///
    /// Default model: `mistral-small-latest`
    pub fn mistral(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("mistral")
            .with_base_url("https://api.mistral.ai/v1")
    }

    /// Perplexity preset.
    ///
    /// Default model: `sonar`
    pub fn perplexity(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("perplexity")
            .with_base_url("https://api.perplexity.ai")
    }

    /// Cerebras preset.
    ///
    /// Default model: `llama-3.3-70b`
    pub fn cerebras(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("cerebras")
            .with_base_url("https://api.cerebras.ai/v1")
    }

    /// SambaNova preset.
    ///
    /// Default model: `Meta-Llama-3.3-70B-Instruct`
    pub fn sambanova(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("sambanova")
            .with_base_url("https://api.sambanova.ai/v1")
    }

    /// xAI (Grok) preset.
    ///
    /// Default model: `grok-3-mini`
    pub fn xai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model).with_provider_name("xai").with_base_url("https://api.x.ai/v1")
    }
}

/// Shared OpenAI-compatible client implementation.
pub struct OpenAICompatible {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    provider_name: String,
    retry_config: RetryConfig,
    reasoning_effort: Option<ReasoningEffort>,
    organization_id: Option<String>,
}

impl OpenAICompatible {
    /// Create a new OpenAI-compatible client.
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, AdkError> {
        let base_url = config.base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        Ok(Self {
            http: reqwest::Client::new(),
            api_key: config.api_key,
            base_url,
            model: config.model,
            provider_name: config.provider_name,
            retry_config: RetryConfig::default(),
            reasoning_effort: config.reasoning_effort,
            organization_id: config.organization_id,
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

/// Build the serialized JSON request body from an `LlmRequest`.
///
/// This is shared between the streaming and non-streaming paths so that
/// request parameter construction is identical regardless of mode.
/// Also used by `AzureOpenAIClient` for consistent request building.
pub(crate) fn build_request_json(
    model: &str,
    request: &LlmRequest,
    reasoning_effort: &Option<ReasoningEffort>,
) -> Result<serde_json::Value, AdkError> {
    let messages: Vec<_> = request.contents.iter().map(convert::content_to_message).collect();

    let mut request_builder = CreateChatCompletionRequestArgs::default();
    request_builder.model(model).messages(messages);

    if !request.tools.is_empty() {
        let tools = convert::convert_tools(&request.tools);
        request_builder.tools(tools);
    }

    if let Some(effort) = reasoning_effort {
        request_builder.reasoning_effort(effort.clone());
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
        .map_err(|e| AdkError::model(format!("failed to build request: {e}")))?;

    let mut body = serde_json::to_value(&openai_request)
        .map_err(|e| AdkError::model(format!("failed to serialize request: {e}")))?;

    // Merge provider-specific extensions from config.extensions["openai"] into
    // the request body.  This allows users to pass provider-specific fields
    // that the typed builder doesn't cover (e.g. provider-specific parameters
    // for OpenAI-compatible APIs like DeepSeek, Together, etc.).
    if let Some(config) = &request.config {
        if let Some(openai_ext) = config.extensions.get("openai") {
            if let (Some(body_obj), Some(ext_obj)) = (body.as_object_mut(), openai_ext.as_object())
            {
                for (key, value) in ext_obj {
                    body_obj.insert(key.clone(), value.clone());
                }
            }
        }
    }

    Ok(body)
}

/// Send an HTTP POST and handle error status codes.
///
/// Returns the raw `reqwest::Response` on success so the caller can decide
/// whether to parse it as JSON (non-streaming) or consume it as an SSE byte
/// stream (streaming).
async fn send_request(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    organization_id: &Option<String>,
    body: &serde_json::Value,
    provider_name: &str,
) -> Result<reqwest::Response, AdkError> {
    let mut http_req = http.post(url).bearer_auth(api_key).json(body);

    if let Some(org_id) = organization_id {
        http_req = http_req.header("OpenAI-Organization", org_id);
    }

    let http_resp = http_req.send().await.map_err(|e| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.openai_compat.request",
            format!("{provider_name} request error: {e}"),
        )
        .with_provider(provider_name)
    })?;

    if !http_resp.status().is_success() {
        let status = http_resp.status();
        let status_code = status.as_u16();
        let body = http_resp.text().await.unwrap_or_default();
        let category = match status_code {
            401 => ErrorCategory::Unauthorized,
            403 => ErrorCategory::Forbidden,
            404 => ErrorCategory::NotFound,
            408 => ErrorCategory::Timeout,
            429 => ErrorCategory::RateLimited,
            503 | 529 => ErrorCategory::Unavailable,
            _ if status_code >= 500 => ErrorCategory::Internal,
            _ => ErrorCategory::InvalidInput,
        };
        return Err(AdkError::new(
            ErrorComponent::Model,
            category,
            "model.openai_compat.api_error",
            format!("{provider_name} API error (HTTP {status}): {body}"),
        )
        .with_upstream_status(status_code)
        .with_provider(provider_name));
    }

    Ok(http_resp)
}

/// Parse a finish_reason string into an ADK `FinishReason`.
fn parse_finish_reason(fr: &str) -> FinishReason {
    match fr {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::MaxTokens,
        "tool_calls" => FinishReason::Stop,
        "content_filter" => FinishReason::Safety,
        "function_call" => FinishReason::Stop,
        _ => FinishReason::Stop,
    }
}

/// Parse usage metadata from a raw SSE chunk JSON value.
fn parse_usage_from_chunk(chunk: &serde_json::Value) -> Option<UsageMetadata> {
    let u = chunk.get("usage")?;
    let prompt_tokens = u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let completion_tokens = u.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let total_tokens = u.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

    let prompt_details = u.get("prompt_tokens_details");
    let completion_details = u.get("completion_tokens_details");

    Some(UsageMetadata {
        prompt_token_count: prompt_tokens,
        candidates_token_count: completion_tokens,
        total_token_count: total_tokens,
        cache_read_input_token_count: prompt_details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        thinking_token_count: completion_details
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        audio_input_token_count: prompt_details
            .and_then(|d| d.get("audio_tokens"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        audio_output_token_count: completion_details
            .and_then(|d| d.get("audio_tokens"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        ..Default::default()
    })
}

#[async_trait]
impl Llm for OpenAICompatible {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let model = self.model.clone();
        let provider_name = self.provider_name.clone();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();
        let reasoning_effort = self.reasoning_effort.clone();
        let organization_id = self.organization_id.clone();

        let usage_span = adk_telemetry::llm_generate_span(&provider_name, &model, stream);

        if stream {
            // ── Streaming path ──────────────────────────────────────
            let response_stream = try_stream! {
                // Build the request JSON and inject streaming fields.
                let mut body = build_request_json(&model, &request_for_retry, &reasoning_effort)?;
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("stream".to_string(), serde_json::json!(true));
                    obj.insert(
                        "stream_options".to_string(),
                        serde_json::json!({"include_usage": true}),
                    );
                }

                let url = format!("{base_url}/chat/completions");

                // Retry covers only the initial HTTP request, not stream consumption.
                let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                    let http = http.clone();
                    let url = url.clone();
                    let api_key = api_key.clone();
                    let organization_id = organization_id.clone();
                    let body = body.clone();
                    let provider_name = provider_name.clone();
                    async move {
                        send_request(&http, &url, &api_key, &organization_id, &body, &provider_name).await
                    }
                })
                .await?;

                // Process SSE byte stream (following DeepSeekClient pattern).
                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();
                let mut tool_call_accumulators: HashMap<u32, (String, String, String)> =
                    HashMap::new();

                while let Some(chunk_result) = byte_stream.next().await {
                    let chunk = chunk_result.map_err(|e| {
                        AdkError::model(format!("stream read error: {e}"))
                    })?;

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    // Process complete SSE lines.
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer = buffer[line_end + 1..].to_string();

                        if line.is_empty() || line == "data: [DONE]" {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            let chunk_json: serde_json::Value = match serde_json::from_str(data) {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!(
                                        "failed to parse SSE chunk: {e} - {data}"
                                    );
                                    continue;
                                }
                            };

                            let choice = match chunk_json.get("choices").and_then(|c| c.get(0)) {
                                Some(c) => c,
                                None => continue,
                            };
                            let delta = match choice.get("delta") {
                                Some(d) => d,
                                None => continue,
                            };

                            let finish_reason_str = choice
                                .get("finish_reason")
                                .and_then(|v| v.as_str())
                                .map(String::from);

                            // Accumulate tool calls by index.
                            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                                for tc in tool_calls {
                                    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                                    let entry = tool_call_accumulators
                                        .entry(index)
                                        .or_insert_with(|| {
                                            let call_id = tc
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            (call_id, String::new(), String::new())
                                        });

                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        if !id.is_empty() {
                                            entry.0 = id.to_string();
                                        }
                                    }

                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                            entry.1 = name.to_string();
                                        }
                                        if let Some(args_chunk) =
                                            func.get("arguments").and_then(|v| v.as_str())
                                        {
                                            entry.2.push_str(args_chunk);
                                        }
                                    }
                                }
                            }

                            // Check for finish_reason → emit final response.
                            if let Some(ref fr) = finish_reason_str {
                                let finish_reason = Some(parse_finish_reason(fr));
                                let usage_metadata = parse_usage_from_chunk(&chunk_json);

                                // Emit accumulated tool calls if any.
                                if !tool_call_accumulators.is_empty() {
                                    let mut sorted_calls: Vec<_> =
                                        tool_call_accumulators.drain().collect();
                                    sorted_calls.sort_by_key(|(idx, _)| *idx);

                                    let parts: Vec<Part> = sorted_calls
                                        .into_iter()
                                        .map(|(_, (id, name, args_str))| {
                                            let args: serde_json::Value =
                                                serde_json::from_str(&args_str)
                                                    .unwrap_or(serde_json::json!({}));
                                            Part::FunctionCall {
                                                name,
                                                args,
                                                id: Some(id),
                                                thought_signature: None,
                                            }
                                        })
                                        .collect();

                                    yield LlmResponse {
                                        content: Some(Content {
                                            role: "model".to_string(),
                                            parts,
                                        }),
                                        usage_metadata,
                                        finish_reason,
                                        citation_metadata: None,
                                        partial: false,
                                        turn_complete: true,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                        provider_metadata: None,
                                    };
                                    continue;
                                }

                                // Final response without tool calls.
                                let mut parts = Vec::new();
                                if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        parts.push(Part::Text { text: text.to_string() });
                                    }
                                }

                                yield LlmResponse {
                                    content: if parts.is_empty() { None } else {
                                        Some(Content {
                                            role: "model".to_string(),
                                            parts,
                                        })
                                    },
                                    usage_metadata,
                                    finish_reason,
                                    citation_metadata: None,
                                    partial: false,
                                    turn_complete: true,
                                    interrupted: false,
                                    error_code: None,
                                    error_message: None,
                                    provider_metadata: None,
                                };
                                continue;
                            }

                            // Emit partial reasoning_content as Part::Thinking.
                            if let Some(reasoning) =
                                delta.get("reasoning_content").and_then(|v| v.as_str())
                            {
                                if !reasoning.is_empty() {
                                    yield LlmResponse {
                                        content: Some(Content {
                                            role: "model".to_string(),
                                            parts: vec![Part::Thinking {
                                                thinking: reasoning.to_string(),
                                                signature: None,
                                            }],
                                        }),
                                        usage_metadata: None,
                                        finish_reason: None,
                                        citation_metadata: None,
                                        partial: true,
                                        turn_complete: false,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                        provider_metadata: None,
                                    };
                                }
                            }

                            // Emit partial text content as Part::Text.
                            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    yield LlmResponse {
                                        content: Some(Content {
                                            role: "model".to_string(),
                                            parts: vec![Part::Text {
                                                text: text.to_string(),
                                            }],
                                        }),
                                        usage_metadata: None,
                                        finish_reason: None,
                                        citation_metadata: None,
                                        partial: true,
                                        turn_complete: false,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                        provider_metadata: None,
                                    };
                                }
                            }
                        }
                    }
                }
            };

            Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
        } else {
            // ── Non-streaming path (preserved identically) ──────────
            let response_stream = try_stream! {
                let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                    let model = model.clone();
                    let provider_name = provider_name.clone();
                    let http = http.clone();
                    let api_key = api_key.clone();
                    let base_url = base_url.clone();
                    let request = request_for_retry.clone();
                    let reasoning_effort = reasoning_effort.clone();
                    let organization_id = organization_id.clone();
                    async move {
                        let body = build_request_json(&model, &request, &reasoning_effort)?;

                        let url = format!("{base_url}/chat/completions");
                        let http_resp =
                            send_request(&http, &url, &api_key, &organization_id, &body, &provider_name)
                                .await?;

                        let raw_json: serde_json::Value = http_resp.json().await.map_err(|e| {
                            AdkError::new(
                                ErrorComponent::Model,
                                ErrorCategory::Internal,
                                "model.openai_compat.parse",
                                format!("{provider_name} response parse error: {e}"),
                            )
                            .with_provider(&provider_name)
                        })?;

                        tracing::debug!(
                            provider = %provider_name,
                            model = %model,
                            has_reasoning = raw_json
                                .pointer("/choices/0/message/reasoning_content")
                                .is_some(),
                            "openai chat completion response"
                        );

                        Ok(raw_json)
                    }
                })
                .await?;

                let adk_response = convert::from_raw_openai_response(&response);
                yield adk_response;
            };

            Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
        }
    }
}
