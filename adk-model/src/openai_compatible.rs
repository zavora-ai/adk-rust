//! Shared OpenAI-compatible provider implementation.

use crate::openai::{OpenAIReasoningEffort, convert};
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, GenericSchemaAdapter, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, Part, SchemaAdapter, SchemaCache, UsageMetadata,
};
use async_openai::types::chat::{
    CreateChatCompletionRequestArgs, ReasoningEffort as OaiReasoningEffort, ResponseFormat,
    ResponseFormatJsonSchema,
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
    pub reasoning_effort: Option<OaiReasoningEffort>,
    /// Whether to allow the model to call multiple tools in a single turn.
    pub parallel_tool_calls: bool,
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
            parallel_tool_calls: true,
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
    pub fn with_reasoning_effort(mut self, effort: OaiReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Set whether parallel tool calls are allowed.
    pub fn with_parallel_tool_calls(mut self, parallel_tool_calls: bool) -> Self {
        self.parallel_tool_calls = parallel_tool_calls;
        self
    }

    // ── Provider presets ─────────────────────────────────────────

    /// Fireworks AI preset.
    ///
    /// Default model: `accounts/fireworks/models/kimi-k2p6`
    pub fn fireworks(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("fireworks")
            .with_base_url("https://api.fireworks.ai/inference/v1")
    }

    /// Together AI preset.
    ///
    /// Default model: `MiniMaxAI/MiniMax-M2.7`
    pub fn together(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("together")
            .with_base_url("https://api.together.xyz/v1")
    }

    /// Mistral AI preset.
    ///
    /// Default model: `mistral-medium-latest`
    pub fn mistral(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("mistral")
            .with_base_url("https://api.mistral.ai/v1")
    }

    /// Perplexity preset.
    ///
    /// Default model: `sonar-pro`
    pub fn perplexity(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("perplexity")
            .with_base_url("https://api.perplexity.ai")
    }

    /// Cerebras preset.
    ///
    /// Default model: `gpt-oss-120b`
    pub fn cerebras(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("cerebras")
            .with_base_url("https://api.cerebras.ai/v1")
    }

    /// SambaNova preset.
    ///
    /// Default model: `gpt-oss-120b`
    pub fn sambanova(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("sambanova")
            .with_base_url("https://api.sambanova.ai/v1")
    }

    /// xAI (Grok) preset.
    ///
    /// Default model: `grok-4.6`
    pub fn xai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model).with_provider_name("xai").with_base_url("https://api.x.ai/v1")
    }

    /// Google Gemini (OpenAI-compatible) preset.
    ///
    /// Targets Gemini's OpenAI-compatibility endpoint, letting you use a Gemini
    /// API key and a Gemini model (e.g. `gemini-3.7-flash`) through the OpenAI
    /// Chat Completions wire format. Use a `GEMINI_API_KEY` for the `api_key`.
    ///
    /// For native Gemini features (thinking levels, server-side tools, the
    /// Interactions API), prefer [`GeminiModel`](crate::gemini::GeminiModel).
    /// This preset is for callers who want a single OpenAI-compatible code path
    /// across providers.
    ///
    /// Default model suggestion: `gemini-3.7-flash`.
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("gemini")
            .with_base_url("https://generativelanguage.googleapis.com/v1beta/openai")
    }

    /// MiniMax preset.
    ///
    /// Default model: `MiniMax-M2.7` (case-sensitive)
    pub fn minimax(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("minimax")
            .with_base_url("https://api.minimax.chat/v1")
    }

    /// ByteDance Doubao (Volcano Engine Ark) preset.
    ///
    /// Default model: `doubao-1-5-pro-256k`
    pub fn bytedance(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("bytedance")
            .with_base_url("https://ark.cn-beijing.volces.com/api/v3")
    }

    /// Zhipu AI (GLM) preset.
    ///
    /// Default model: `glm-5.2`
    pub fn zhipu(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("zhipu")
            .with_base_url("https://open.bigmodel.cn/api/paas/v4")
    }

    /// Baidu ERNIE (Qianfan) preset via OpenAI-compatible endpoint.
    ///
    /// Default model: `ernie-5.1`
    ///
    /// Note: Uses the Qianfan OpenAI-compatible endpoint. For the native
    /// Qianfan API with OAuth2 token exchange, use a dedicated client.
    pub fn baidu(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("baidu")
            .with_base_url("https://qianfan.baidubce.com/v2")
    }

    /// Cohere preset via OpenAI-compatible endpoint.
    ///
    /// Default model: `command-a-plus-05-2026`
    ///
    /// Note: For full Cohere features (citations, connectors, RAG), use
    /// the native Cohere API. This preset provides basic chat completions.
    pub fn cohere(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("cohere")
            .with_base_url("https://api.cohere.com/compatibility/v1")
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
    reasoning_effort: Option<OpenAIReasoningEffort>,
    organization_id: Option<String>,
    parallel_tool_calls: bool,
}

impl OpenAICompatible {
    /// Create a new OpenAI-compatible client.
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, AdkError> {
        let reasoning_effort = config.reasoning_effort.clone().map(|effort| match effort {
            OaiReasoningEffort::None => OpenAIReasoningEffort::None,
            OaiReasoningEffort::Minimal => OpenAIReasoningEffort::Minimal,
            OaiReasoningEffort::Low => OpenAIReasoningEffort::Low,
            OaiReasoningEffort::Medium => OpenAIReasoningEffort::Medium,
            OaiReasoningEffort::High => OpenAIReasoningEffort::High,
            OaiReasoningEffort::Xhigh => OpenAIReasoningEffort::XHigh,
        });
        Self::new_with_reasoning_effort(config, reasoning_effort)
    }

    /// Create an OpenAI-compatible client with the complete OpenAI reasoning vocabulary.
    ///
    /// This supports newer values such as `none`, `xhigh`, and `max` without changing
    /// the backward-compatible [`OpenAICompatibleConfig::reasoning_effort`] field type.
    pub fn new_with_reasoning_effort(
        config: OpenAICompatibleConfig,
        reasoning_effort: Option<OpenAIReasoningEffort>,
    ) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete(&config.provider_name, &config.model);
        let base_url = config.base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        Ok(Self {
            http: reqwest::Client::new(),
            api_key: config.api_key,
            base_url,
            model: config.model,
            provider_name: config.provider_name,
            retry_config: RetryConfig::default(),
            reasoning_effort,
            organization_id: config.organization_id,
            parallel_tool_calls: config.parallel_tool_calls,
        })
    }

    /// Set the retry configuration (builder pattern).
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set the retry configuration (mutable reference).
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Returns the current retry configuration.
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
    reasoning_effort: &Option<OpenAIReasoningEffort>,
    parallel_tool_calls: bool,
    adapter: &dyn SchemaAdapter,
    cache: &SchemaCache,
) -> Result<serde_json::Value, AdkError> {
    let bare_model = model.strip_prefix("models/").unwrap_or(model);
    if matches!(bare_model, "gemini-3.6-flash" | "gemini-3.7-flash")
        && request.config.as_ref().is_some_and(|config| {
            config.temperature.is_some() || config.top_p.is_some() || config.top_k.is_some()
        })
    {
        return Err(AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.gemini.sampling_unsupported",
            format!(
                "{bare_model} does not accept temperature, top_p, or top_k; remove explicit sampling parameters"
            ),
        )
        .with_provider("gemini"));
    }
    let messages: Vec<_> = request.contents.iter().map(convert::content_to_message).collect();

    let mut request_builder = CreateChatCompletionRequestArgs::default();
    request_builder.model(model).messages(messages);

    if !request.tools.is_empty() {
        let tools = convert::convert_tools(&request.tools, adapter, cache);
        request_builder.tools(tools);
        // OpenAI defaults parallel_tool_calls to true.
        request_builder.parallel_tool_calls(parallel_tool_calls);
    }

    if let Some(effort) = reasoning_effort.and_then(to_oai_reasoning_effort) {
        request_builder.reasoning_effort(effort);
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
                schema: schema_with_strict,
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

    if matches!(reasoning_effort, Some(OpenAIReasoningEffort::Max)) {
        body["reasoning_effort"] = serde_json::Value::String("max".to_string());
    }

    // Merge provider-specific extensions from config.extensions["openai"] into
    // the request body.  This allows users to pass provider-specific fields
    // that the typed builder doesn't cover (e.g. provider-specific parameters
    // for OpenAI-compatible APIs like DeepSeek, Together, etc.).
    if let Some(config) = &request.config
        && let Some(openai_ext) = config.extensions.get("openai")
        && let (Some(body_obj), Some(ext_obj)) = (body.as_object_mut(), openai_ext.as_object())
    {
        for (key, value) in ext_obj {
            body_obj.insert(key.clone(), value.clone());
        }
    }

    Ok(body)
}

fn to_oai_reasoning_effort(effort: OpenAIReasoningEffort) -> Option<OaiReasoningEffort> {
    match effort {
        OpenAIReasoningEffort::None => Some(OaiReasoningEffort::None),
        OpenAIReasoningEffort::Minimal => Some(OaiReasoningEffort::Minimal),
        OpenAIReasoningEffort::Low => Some(OaiReasoningEffort::Low),
        OpenAIReasoningEffort::Medium => Some(OaiReasoningEffort::Medium),
        OpenAIReasoningEffort::High => Some(OaiReasoningEffort::High),
        OpenAIReasoningEffort::XHigh => Some(OaiReasoningEffort::Xhigh),
        OpenAIReasoningEffort::Max => None,
    }
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

/// Accumulate string deltas while treating structured values as complete snapshots.
fn append_tool_call_arguments(accumulator: &mut String, arguments: &serde_json::Value) {
    match arguments {
        serde_json::Value::String(fragment) => accumulator.push_str(fragment),
        serde_json::Value::Null => {}
        structured => {
            let empty_snapshot = matches!(structured, serde_json::Value::Object(fields) if fields.is_empty())
                || matches!(structured, serde_json::Value::Array(items) if items.is_empty());
            if !empty_snapshot {
                accumulator.clear();
                accumulator.push_str(&structured.to_string());
            }
        }
    }
}

fn parse_tool_call_arguments(
    provider_name: &str,
    tool_name: &str,
    arguments: &str,
) -> Result<serde_json::Value, AdkError> {
    let encoded = serde_json::Value::String(arguments.to_owned());
    convert::decode_tool_call_arguments(Some(&encoded)).map_err(|error| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.openai_compat.invalid_tool_arguments",
            format!(
                "{provider_name} returned invalid JSON arguments for tool '{tool_name}': {error}"
            ),
        )
        .with_provider(provider_name)
    })
}

/// Parse usage metadata from a raw SSE chunk JSON value.
fn parse_usage_from_chunk(chunk: &serde_json::Value) -> Option<UsageMetadata> {
    chunk.get("usage").and_then(convert::usage_metadata_from_raw)
}

#[async_trait]
impl Llm for OpenAICompatible {
    fn name(&self) -> &str {
        &self.model
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
        let model = self.model.clone();
        let provider_name = self.provider_name.clone();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let retry_config = self.retry_config.clone();
        let reasoning_effort = self.reasoning_effort;
        let organization_id = self.organization_id.clone();

        // Normalize tool schemas at request time using the schema adapter.
        let adapter = self.schema_adapter();
        use std::sync::LazyLock;
        static SCHEMA_CACHE: LazyLock<SchemaCache> =
            LazyLock::new(|| SchemaCache::for_adapter(std::sync::Arc::new(GenericSchemaAdapter)));
        let request_body = build_request_json(
            &model,
            &request,
            &reasoning_effort,
            self.parallel_tool_calls,
            adapter,
            &SCHEMA_CACHE,
        )?;

        let usage_span = adk_telemetry::llm_generate_span(&provider_name, &model, stream);

        if stream {
            // ── Streaming path ──────────────────────────────────────
            let response_stream = try_stream! {
                // Inject streaming fields into the pre-built request body.
                let mut body = request_body.clone();
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
                let mut text_tool_buffer = crate::tool_call_parser::ToolCallBuffer::new();
                let mut pending_final_response: Option<LlmResponse> = None;

                while let Some(chunk_result) = byte_stream.next().await {
                    let chunk = chunk_result.map_err(|e| {
                        AdkError::model(format!("stream read error: {e}"))
                    })?;

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    // Process complete SSE lines.
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer = buffer[line_end + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if line == "data: [DONE]" {
                            if let Some(response) = pending_final_response.take() {
                                yield response;
                            }
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
                            let usage_metadata = parse_usage_from_chunk(&chunk_json);

                            let choice = match chunk_json.get("choices").and_then(|c| c.get(0)) {
                                Some(c) => c,
                                None => {
                                    if let Some(usage_metadata) = usage_metadata
                                        && let Some(mut response) = pending_final_response.take()
                                    {
                                        response.usage_metadata = Some(usage_metadata);
                                        yield response;
                                    }
                                    continue;
                                }
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

                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str())
                                        && !id.is_empty() {
                                            entry.0 = id.to_string();
                                        }

                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                            entry.1 = name.to_string();
                                        }
                                        if let Some(arguments) = func.get("arguments") {
                                            append_tool_call_arguments(&mut entry.2, arguments);
                                        }
                                    }
                                }
                            }

                            // Check for finish_reason → emit final response.
                            if let Some(ref fr) = finish_reason_str {
                                let finish_reason = Some(parse_finish_reason(fr));

                                // Emit accumulated tool calls if any.
                                if !tool_call_accumulators.is_empty() {
                                    let mut sorted_calls: Vec<_> =
                                        tool_call_accumulators.drain().collect();
                                    sorted_calls.sort_by_key(|(idx, _)| *idx);

                                    let parts: Vec<Part> = sorted_calls
                                        .into_iter()
                                        .map(|(_, (id, name, args_str))| {
                                            let args = parse_tool_call_arguments(
                                                &provider_name,
                                                &name,
                                                &args_str,
                                            )?;
                                            Ok(Part::FunctionCall {
                                                name,
                                                args,
                                                id: Some(id),
                                                thought_signature: None,
                                            })
                                        })
                                        .collect::<Result<Vec<_>, AdkError>>()?;

                                    let response = LlmResponse {
                                        content: Some(Content {
                                            role: "model".to_string(),
                                            parts,
                                        }),
                                        usage_metadata,
                                        finish_reason,
                                        citation_metadata: None,
                                        partial: false,
                                        // Tool-call turns are not complete — tool
                                        // results must still be processed (issue #401).
                                        turn_complete: false,
                                        interrupted: false,
                                        error_code: None,
                                        error_message: None,
                                        provider_metadata: None,
                                        interaction_id: None,
                                    };
                                    if response.usage_metadata.is_some() {
                                        yield response;
                                    } else {
                                        pending_final_response = Some(response);
                                    }
                                    continue;
                                }

                                // Final response without tool calls.
                                let mut parts = Vec::new();
                                if let Some(text) = delta.get("content").and_then(|v| v.as_str())
                                    && !text.is_empty() {
                                        parts.push(Part::Text { text: text.to_string() });
                                    }

                                let response = LlmResponse {
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
                                    interaction_id: None,
                                };
                                if response.usage_metadata.is_some() {
                                    yield response;
                                } else {
                                    pending_final_response = Some(response);
                                }
                                continue;
                            }

                            // Emit partial reasoning_content as Part::Thinking.
                            // Fallback to "reasoning" field for OpenRouter, Kilo Gateway, SambaNova, Cerebras, Groq
                            let reasoning = delta.get("reasoning_content")
                                .or_else(|| delta.get("reasoning"))
                                .and_then(|v| v.as_str());
                            if let Some(reasoning) = reasoning
                                && !reasoning.is_empty() {
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
                                        interaction_id: None,
                                    };
                                }

                            // Emit partial text content via tool call buffer.
                            // The buffer detects <tool_call> tags split across chunks
                            // and converts them to Part::FunctionCall.
                            if let Some(text) = delta.get("content").and_then(|v| v.as_str())
                                && !text.is_empty() {
                                    match text_tool_buffer.push(text) {
                                        crate::tool_call_parser::BufferAction::Emit(parts) => {
                                            for part in parts {
                                                let is_tool = matches!(part, Part::FunctionCall { .. });
                                                yield LlmResponse {
                                                    content: Some(Content {
                                                        role: "model".to_string(),
                                                        parts: vec![part],
                                                    }),
                                                    usage_metadata: None,
                                                    finish_reason: None,
                                                    citation_metadata: None,
                                                    partial: !is_tool,
                                                    turn_complete: false,
                                                    interrupted: false,
                                                    error_code: None,
                                                    error_message: None,
                                                    provider_metadata: None,
                                                    interaction_id: None,
                                                };
                                            }
                                        }
                                        crate::tool_call_parser::BufferAction::Buffering => {
                                            // Still accumulating a potential tool call
                                        }
                                    }
                                }
                        }
                    }
                }

                if let Some(response) = pending_final_response.take() {
                    yield response;
                }

                // Flush any remaining buffered content from the tool call buffer
                for part in text_tool_buffer.flush() {
                    let is_tool = matches!(part, Part::FunctionCall { .. });
                    yield LlmResponse {
                        content: Some(Content {
                            role: "model".to_string(),
                            parts: vec![part],
                        }),
                        usage_metadata: None,
                        finish_reason: if is_tool { Some(adk_core::FinishReason::Stop) } else { None },
                        citation_metadata: None,
                        partial: !is_tool,
                        turn_complete: false,
                        interrupted: false,
                        error_code: None,
                        error_message: None,
                        provider_metadata: None,
                        interaction_id: None,
                    };
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
                    let body = request_body.clone();
                    let organization_id = organization_id.clone();
                    async move {
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

                let adk_response = convert::from_raw_openai_response(&response).map_err(|error| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::Internal,
                        "model.openai_compat.invalid_tool_arguments",
                        format!("{provider_name} returned {error}"),
                    )
                    .with_provider(&provider_name)
                })?;
                yield adk_response;
            };

            Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{GenericSchemaAdapter, LlmRequest, SchemaCache};
    use std::sync::Arc;

    #[test]
    fn test_parallel_tool_calls_config() {
        let config =
            OpenAICompatibleConfig::new("test-key", "test-model").with_parallel_tool_calls(false);

        assert!(!config.parallel_tool_calls, "parallel_tool_calls should be false in config");

        let client = OpenAICompatible::new(config).expect("client creation failed");
        assert!(!client.parallel_tool_calls, "parallel_tool_calls should be false in client");
    }

    #[test]
    fn test_parallel_tool_calls_default() {
        let config = OpenAICompatibleConfig::new("test-key", "test-model");

        assert!(config.parallel_tool_calls, "parallel_tool_calls should default to true");

        let client = OpenAICompatible::new(config).expect("client creation failed");
        assert!(client.parallel_tool_calls, "parallel_tool_calls should default to true in client");
    }

    #[test]
    fn streamed_tool_arguments_require_valid_json() {
        let parsed =
            parse_tool_call_arguments("compatible-provider", "bash", r#"{"command":"pwd"}"#)
                .expect("valid arguments should parse");
        assert_eq!(parsed["command"], "pwd");

        assert_eq!(
            parse_tool_call_arguments("compatible-provider", "no_args", "")
                .expect("an empty compatible payload should normalize"),
            serde_json::json!({})
        );
        assert_eq!(
            parse_tool_call_arguments("compatible-provider", "no_args", "   ")
                .expect("a whitespace-only compatible payload should normalize"),
            serde_json::json!({})
        );
        assert_eq!(
            parse_tool_call_arguments("compatible-provider", "no_args", "[]")
                .expect("an empty array compatible payload should normalize"),
            serde_json::json!({})
        );

        let error = parse_tool_call_arguments("compatible-provider", "bash", r#"{"command":"#)
            .expect_err("truncated arguments must remain invalid");
        assert_eq!(error.component, ErrorComponent::Model);
        assert_eq!(error.category, ErrorCategory::Internal);
        assert_eq!(error.code, "model.openai_compat.invalid_tool_arguments");
        assert_eq!(error.details.provider.as_deref(), Some("compatible-provider"));

        let error = parse_tool_call_arguments("compatible-provider", "bash", r#"["pwd"]"#)
            .expect_err("non-empty array arguments must remain invalid");
        assert_eq!(error.code, "model.openai_compat.invalid_tool_arguments");
    }

    #[test]
    fn streamed_structured_tool_arguments_are_canonicalized() {
        let mut arguments = String::new();
        append_tool_call_arguments(&mut arguments, &serde_json::json!({"command": "pwd"}));

        assert_eq!(
            parse_tool_call_arguments("compatible-provider", "bash", &arguments)
                .expect("a structured object should normalize"),
            serde_json::json!({"command": "pwd"})
        );
    }

    #[test]
    fn streamed_empty_snapshot_before_string_fragments_is_ignored() {
        for placeholder in [serde_json::json!({}), serde_json::json!([])] {
            let mut arguments = String::new();
            append_tool_call_arguments(&mut arguments, &placeholder);
            assert_eq!(
                parse_tool_call_arguments("compatible-provider", "no_args", &arguments)
                    .expect("an empty snapshot should remain a no-argument call"),
                serde_json::json!({})
            );

            append_tool_call_arguments(&mut arguments, &serde_json::json!(r#"{"command""#));
            append_tool_call_arguments(&mut arguments, &serde_json::json!(r#": "pwd"}"#));

            assert_eq!(
                parse_tool_call_arguments("compatible-provider", "bash", &arguments)
                    .expect("empty snapshots must not prefix string fragments"),
                serde_json::json!({"command": "pwd"})
            );
        }
    }

    #[test]
    fn gpt_56_chat_tools_preserve_configured_reasoning() {
        let adapter = GenericSchemaAdapter;
        let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
        let mut request = LlmRequest {
            model: crate::catalog::OPENAI_DEFAULT.to_string(),
            contents: Vec::new(),
            config: None,
            tools: HashMap::new(),
            previous_response_id: None,
        };
        request.tools.insert(
            "lookup".to_string(),
            serde_json::json!({
                "description": "Look up a record",
                "parameters": {"type": "object", "properties": {}}
            }),
        );

        for (configured_effort, expected) in [
            (None, None),
            (Some(OpenAIReasoningEffort::Medium), Some("medium")),
            (Some(OpenAIReasoningEffort::Max), Some("max")),
        ] {
            let body = build_request_json(
                crate::catalog::OPENAI_DEFAULT,
                &request,
                &configured_effort,
                true,
                &adapter,
                &cache,
            )
            .expect("request should build");

            assert_eq!(body.get("reasoning_effort").and_then(serde_json::Value::as_str), expected);
        }
    }

    #[test]
    fn gpt_56_without_tools_keeps_server_reasoning_default() {
        let adapter = GenericSchemaAdapter;
        let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
        let request = LlmRequest {
            model: crate::catalog::OPENAI_DEFAULT.to_string(),
            contents: Vec::new(),
            config: None,
            tools: HashMap::new(),
            previous_response_id: None,
        };

        let body = build_request_json(
            crate::catalog::OPENAI_DEFAULT,
            &request,
            &None,
            true,
            &adapter,
            &cache,
        )
        .expect("request should build");

        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn gpt_56_chat_serializes_max_reasoning() {
        let adapter = GenericSchemaAdapter;
        let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
        let request = LlmRequest {
            model: crate::catalog::OPENAI_DEFAULT.to_string(),
            contents: Vec::new(),
            config: None,
            tools: HashMap::new(),
            previous_response_id: None,
        };

        let body = build_request_json(
            crate::catalog::OPENAI_DEFAULT,
            &request,
            &Some(OpenAIReasoningEffort::Max),
            true,
            &adapter,
            &cache,
        )
        .expect("request should build");

        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn gemini_37_compatible_path_rejects_sampling_locally() {
        let mut request = LlmRequest::new("gemini-3.7-flash", Vec::new());
        request.config =
            Some(adk_core::GenerateContentConfig { top_p: Some(0.9), ..Default::default() });
        let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));

        let error = build_request_json(
            "gemini-3.7-flash",
            &request,
            &None,
            true,
            &GenericSchemaAdapter,
            &cache,
        )
        .expect_err("sampling should be rejected before network I/O");

        assert_eq!(error.code, "model.gemini.sampling_unsupported");
    }

    #[test]
    fn gemini_preset_sets_endpoint_and_provider() {
        let config = OpenAICompatibleConfig::gemini("test-key", "gemini-3.5-flash");
        assert_eq!(config.provider_name, "gemini");
        assert_eq!(config.model, "gemini-3.5-flash");
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );
        assert_eq!(config.api_key, "test-key");
    }

    #[test]
    fn gemini_preset_supports_reasoning_effort() {
        // Gemini's OpenAI-compat layer maps reasoning_effort onto thinking levels.
        let config = OpenAICompatibleConfig::gemini("k", "gemini-3.5-flash")
            .with_reasoning_effort(OaiReasoningEffort::Low);
        assert_eq!(config.reasoning_effort, Some(OaiReasoningEffort::Low));
    }

    #[test]
    fn gemini_preset_builds_client() {
        let config = OpenAICompatibleConfig::gemini("k", "gemini-3.5-flash");
        let client = OpenAICompatible::new(config).expect("client builds");
        assert_eq!(client.name(), "gemini-3.5-flash");
    }
}
