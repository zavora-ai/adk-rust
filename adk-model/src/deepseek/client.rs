//! DeepSeek client implementation.
//!
//! Supports DeepSeek V4 models (`deepseek-v4-pro`, `deepseek-v4-flash`) and
//! legacy models (`deepseek-chat`, `deepseek-reasoner`).

use super::config::{DeepSeekConfig, ThinkingMode};
use super::convert::{self, ChatCompletionRequest, ChatCompletionResponse, ThinkingConfig};
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

/// DeepSeek client for V4 and legacy models.
///
/// # V4 Models
///
/// ```rust,ignore
/// use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort};
///
/// // V4 Pro with max reasoning
/// let pro = DeepSeekClient::new(
///     DeepSeekConfig::v4_pro("api-key")
///         .with_reasoning_effort(ReasoningEffort::Max)
/// )?;
///
/// // V4 Flash (fast, no thinking by default)
/// let flash = DeepSeekClient::v4_flash("api-key")?;
/// ```
///
/// # Legacy Models
///
/// ```rust,ignore
/// // Still works — backward compatible
/// let chat = DeepSeekClient::chat("api-key")?;
/// let reasoner = DeepSeekClient::reasoner("api-key")?;
/// ```
pub struct DeepSeekClient {
    client: Client,
    config: DeepSeekConfig,
    retry_config: RetryConfig,
}

impl DeepSeekClient {
    /// Create a new DeepSeek client.
    pub fn new(config: DeepSeekConfig) -> Result<Self, AdkError> {
        let client = Client::builder()
            .build()
            .map_err(|e| AdkError::model(format!("failed to create HTTP client: {e}")))?;

        Ok(Self { client, config, retry_config: RetryConfig::default() })
    }

    /// Create a client for `deepseek-v4-pro` (strongest reasoning, thinking enabled).
    pub fn v4_pro(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::v4_pro(api_key))
    }

    /// Create a client for `deepseek-v4-flash` (fast, cost-efficient).
    pub fn v4_flash(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::v4_flash(api_key))
    }

    /// Create a client for `deepseek-chat` model (legacy).
    pub fn chat(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::chat(api_key))
    }

    /// Create a client for `deepseek-reasoner` model with thinking enabled (legacy).
    pub fn reasoner(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::reasoner(api_key))
    }

    /// Set retry configuration.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set retry configuration (mutable).
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Get the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    /// Build the API URL for chat completions.
    fn api_url(&self) -> String {
        let base = self.config.effective_base_url();
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    /// Build a chat completion request from an LLM request.
    fn build_request(&self, request: &LlmRequest, stream: bool) -> ChatCompletionRequest {
        let messages: Vec<_> = request.contents.iter().map(convert::content_to_message).collect();

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(convert::convert_tools(&request.tools, self.config.strict_tools))
        };

        // Get generation config
        let temperature = request.config.as_ref().and_then(|c| c.temperature);
        let top_p = request.config.as_ref().and_then(|c| c.top_p);
        let max_tokens = request
            .config
            .as_ref()
            .and_then(|c| c.max_output_tokens)
            .map(|t| t as u32)
            .or(self.config.max_tokens);

        // Build thinking config from the new ThinkingMode or legacy bool
        let thinking = match self.config.thinking {
            Some(ThinkingMode::Enabled) => Some(ThinkingConfig::enabled()),
            Some(ThinkingMode::Disabled) => Some(ThinkingConfig::disabled()),
            None => {
                if self.config.thinking_enabled {
                    Some(ThinkingConfig::enabled())
                } else {
                    None
                }
            }
        };

        // Reasoning effort
        let reasoning_effort = self.config.reasoning_effort.map(|e| e.to_string());

        ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature,
            top_p,
            max_tokens,
            stream: Some(stream),
            tools,
            response_format: None,
            thinking,
            reasoning_effort,
            stop: None,
        }
    }
}

#[async_trait]
impl Llm for DeepSeekClient {
    fn name(&self) -> &str {
        &self.config.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let usage_span = adk_telemetry::llm_generate_span("deepseek", &self.config.model, stream);
        let api_url = self.api_url();
        let api_key = self.config.api_key.clone();
        let chat_request = self.build_request(&request, stream);
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();
        let thinking_enabled = self.config.is_thinking_enabled();

        let response_stream = try_stream! {
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let client = client.clone();
                let api_url = api_url.clone();
                let api_key = api_key.clone();
                let chat_request = chat_request.clone();
                async move {
                    let response = client
                        .post(&api_url)
                        .header("Authorization", format!("Bearer {api_key}"))
                        .header("Content-Type", "application/json")
                        .json(&chat_request)
                        .send()
                        .await
                        .map_err(|e| AdkError::new(
                            ErrorComponent::Model,
                            ErrorCategory::Unavailable,
                            "model.deepseek.request",
                            format!("DeepSeek API request failed: {e}"),
                        ).with_provider("deepseek"))?;

                    if !response.status().is_success() {
                        let status = response.status();
                        let status_code = status.as_u16();
                        let error_text = response.text().await.unwrap_or_default();
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
                            "model.deepseek.api_error",
                            format!("DeepSeek API error (HTTP {status}): {error_text}"),
                        ).with_upstream_status(status_code).with_provider("deepseek"));
                    }

                    Ok(response)
                }
            })
            .await?;

            if stream {
                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();
                let mut tool_call_accumulators: std::collections::HashMap<u32, (String, String, String)> =
                    std::collections::HashMap::new();
                let mut reasoning_buffer = String::new();

                while let Some(chunk_result) = byte_stream.next().await {
                    let chunk = chunk_result
                        .map_err(|e| AdkError::model(format!("stream read error: {e}")))?;

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer = buffer[line_end + 1..].to_string();

                        if line.is_empty() || line == "data: [DONE]" {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            match serde_json::from_str::<ChatCompletionResponse>(data) {
                                Ok(chunk_response) => {
                                    if let Some(choice) = chunk_response.choices.first() {
                                        if let Some(delta) = &choice.delta {
                                            // Accumulate reasoning content
                                            if let Some(reasoning) = &delta.reasoning_content {
                                                if !reasoning.is_empty() {
                                                    reasoning_buffer.push_str(reasoning);
                                                    if thinking_enabled {
                                                        yield LlmResponse {
                                                            content: Some(adk_core::Content {
                                                                role: "model".to_string(),
                                                                parts: vec![Part::Thinking {
                                                                    thinking: reasoning.clone(),
                                                                    signature: None,
                                                                }],
                                                            }),
                                                            partial: true,
                                                            turn_complete: false,
                                                            ..Default::default()
                                                        };
                                                    }
                                                }
                                            }

                                            // Handle tool calls
                                            if let Some(tool_calls) = &delta.tool_calls {
                                                for tc in tool_calls {
                                                    let index = tc.index;
                                                    let entry = tool_call_accumulators
                                                        .entry(index)
                                                        .or_insert_with(|| {
                                                            let call_id = tc.id.clone().unwrap_or_else(|| {
                                                                format!("call_{index}")
                                                            });
                                                            (call_id, String::new(), String::new())
                                                        });
                                                    if let Some(id) = &tc.id {
                                                        entry.0.clone_from(id);
                                                    }
                                                    if let Some(func) = &tc.function {
                                                        if let Some(name) = &func.name {
                                                            entry.1.clone_from(name);
                                                        }
                                                        if let Some(args_chunk) = &func.arguments {
                                                            entry.2.push_str(args_chunk);
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // Check for finish
                                        if choice.finish_reason.is_some() {
                                            let finish_reason = choice.finish_reason.as_ref().map(|fr| {
                                                match fr.as_str() {
                                                    "stop" => FinishReason::Stop,
                                                    "length" => FinishReason::MaxTokens,
                                                    "tool_calls" => FinishReason::Stop,
                                                    "content_filter" => FinishReason::Safety,
                                                    _ => FinishReason::Stop,
                                                }
                                            });

                                            if !tool_call_accumulators.is_empty() {
                                                let mut sorted_calls: Vec<_> =
                                                    tool_call_accumulators.drain().collect();
                                                sorted_calls.sort_by_key(|(idx, _)| *idx);
                                                let tool_calls: Vec<_> = sorted_calls
                                                    .into_iter()
                                                    .map(|(_, (id, name, args_str))| {
                                                        let args: Value =
                                                            serde_json::from_str(&args_str)
                                                                .unwrap_or(serde_json::json!({}));
                                                        (id, name, args)
                                                    })
                                                    .collect();
                                                yield convert::create_tool_call_response(
                                                    tool_calls,
                                                    finish_reason,
                                                );
                                                continue;
                                            }

                                            let mut parts = Vec::new();
                                            if let Some(delta) = &choice.delta {
                                                if let Some(text) = &delta.content {
                                                    if !text.is_empty() {
                                                        parts.push(Part::Text { text: text.clone() });
                                                    }
                                                }
                                            }

                                            yield LlmResponse {
                                                content: if parts.is_empty() {
                                                    None
                                                } else {
                                                    Some(adk_core::Content {
                                                        role: "model".to_string(),
                                                        parts,
                                                    })
                                                },
                                                usage_metadata: chunk_response.usage.map(|u| {
                                                    adk_core::UsageMetadata {
                                                        prompt_token_count: u.prompt_tokens as i32,
                                                        candidates_token_count: u.completion_tokens as i32,
                                                        total_token_count: u.total_tokens as i32,
                                                        thinking_token_count: u.reasoning_tokens.map(|t| t as i32),
                                                        cache_read_input_token_count: u.prompt_cache_hit_tokens.map(|t| t as i32),
                                                        cache_creation_input_token_count: u.prompt_cache_miss_tokens.map(|t| t as i32),
                                                        ..Default::default()
                                                    }
                                                }),
                                                finish_reason,
                                                partial: false,
                                                turn_complete: true,
                                                ..Default::default()
                                            };
                                        } else {
                                            // Emit partial text content
                                            if let Some(delta) = &choice.delta {
                                                if let Some(text) = &delta.content {
                                                    if !text.is_empty() {
                                                        yield LlmResponse {
                                                            content: Some(adk_core::Content {
                                                                role: "model".to_string(),
                                                                parts: vec![Part::Text {
                                                                    text: text.clone(),
                                                                }],
                                                            }),
                                                            partial: true,
                                                            turn_complete: false,
                                                            ..Default::default()
                                                        };
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("failed to parse DeepSeek chunk: {e} - {data}");
                                }
                            }
                        }
                    }
                }
            } else {
                // Non-streaming mode
                let response_text = response.text().await
                    .map_err(|e| AdkError::model(format!("failed to read response: {e}")))?;

                let chat_response: ChatCompletionResponse = serde_json::from_str(&response_text)
                    .map_err(|e| AdkError::model(format!(
                        "failed to parse response: {e} - {response_text}"
                    )))?;

                yield convert::from_response(&chat_response);
            }
        };

        Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
    }
}
