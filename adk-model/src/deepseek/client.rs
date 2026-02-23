//! DeepSeek client implementation.

use super::config::{DEEPSEEK_API_BASE, DeepSeekConfig};
use super::convert::{self, ChatCompletionRequest, ChatCompletionResponse, ThinkingConfig};
use crate::retry::{
    RetryConfig, execute_with_retry, is_retryable_model_error, is_retryable_status_code,
};
use adk_core::{AdkError, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

/// DeepSeek client for deepseek-chat and deepseek-reasoner models.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
///
/// // Basic chat model
/// let client = DeepSeekClient::new(DeepSeekConfig::chat(
///     std::env::var("DEEPSEEK_API_KEY").unwrap()
/// ))?;
///
/// // Reasoner model with thinking enabled
/// let reasoner = DeepSeekClient::new(DeepSeekConfig::reasoner(
///     std::env::var("DEEPSEEK_API_KEY").unwrap()
/// ))?;
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
            .map_err(|e| AdkError::Model(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client, config, retry_config: RetryConfig::default() })
    }

    /// Create a client for deepseek-chat model.
    pub fn chat(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::chat(api_key))
    }

    /// Create a client for deepseek-reasoner model with thinking enabled.
    pub fn reasoner(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(DeepSeekConfig::reasoner(api_key))
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

    /// Build the API URL for chat completions.
    fn api_url(&self) -> String {
        let base = self.config.base_url.as_deref().unwrap_or(DEEPSEEK_API_BASE);
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    /// Build a chat completion request from an LLM request.
    fn build_request(&self, request: &LlmRequest, stream: bool) -> ChatCompletionRequest {
        let messages: Vec<_> = request.contents.iter().map(convert::content_to_message).collect();

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(convert::convert_tools(&request.tools))
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

        // Enable thinking mode if configured
        let thinking =
            if self.config.thinking_enabled { Some(ThinkingConfig::enabled()) } else { None };

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
        let api_url = self.api_url();
        let api_key = self.config.api_key.clone();
        let chat_request = self.build_request(&request, stream);
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();
        let thinking_enabled = self.config.thinking_enabled;

        let response_stream = try_stream! {
            // Retries only cover request setup/execution. Stream failures after start are surfaced
            // directly and are not auto-replayed.
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let client = client.clone();
                let api_url = api_url.clone();
                let api_key = api_key.clone();
                let chat_request = chat_request.clone();
                async move {
                    let response = client
                        .post(&api_url)
                        .header("Authorization", format!("Bearer {}", api_key))
                        .header("Content-Type", "application/json")
                        .json(&chat_request)
                        .send()
                        .await
                        .map_err(|e| AdkError::Model(format!("DeepSeek API request failed: {}", e)))?;

                    if !response.status().is_success() {
                        let status = response.status();
                        let error_text = response.text().await.unwrap_or_default();
                        let retryability = if is_retryable_status_code(status.as_u16()) {
                            "retryable"
                        } else {
                            "non-retryable"
                        };
                        return Err(AdkError::Model(format!(
                            "DeepSeek API error ({}, {}): {}",
                            status, retryability, error_text
                        )));
                    }

                    Ok(response)
                }
            })
            .await?;

            if stream {
                // Streaming mode - process SSE events
                let mut byte_stream = response.bytes_stream();
                let mut buffer = String::new();

                // For tool calls, accumulate across chunks
                let mut tool_call_accumulators: std::collections::HashMap<u32, (String, String, String)> =
                    std::collections::HashMap::new();

                // For reasoning content, accumulate across chunks
                let mut reasoning_buffer = String::new();

                while let Some(chunk_result) = byte_stream.next().await {
                    let chunk = chunk_result
                        .map_err(|e| AdkError::Model(format!("Stream read error: {}", e)))?;

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    // Process complete SSE events
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
                                        // Handle tool call accumulation
                                        if let Some(delta) = &choice.delta {
                                            // Accumulate reasoning content
                                            if let Some(reasoning) = &delta.reasoning_content {
                                                if !reasoning.is_empty() {
                                                    reasoning_buffer.push_str(reasoning);

                                                    // Emit reasoning as Part::Thinking if thinking is enabled
                                                    if thinking_enabled {
                                                        yield LlmResponse {
                                                            content: Some(adk_core::Content {
                                                                role: "model".to_string(),
                                                                parts: vec![Part::Thinking {
                                                                    thinking: reasoning.clone(),
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
                                                                format!("call_{}", index)
                                                            });
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

                                            // Emit tool calls if any
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

                                            // Emit final response
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
                                                        ..Default::default()
                                                    }
                                                }),
                                                finish_reason,
                                                citation_metadata: None,
                                                partial: false,
                                                turn_complete: true,
                                                interrupted: false,
                                                error_code: None,
                                                error_message: None,
                                            };
                                        } else {
                                            // Emit partial text content (non-reasoning)
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
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse DeepSeek chunk: {} - {}", e, data);
                                }
                            }
                        }
                    }
                }
            } else {
                // Non-streaming mode
                let response_text = response.text().await
                    .map_err(|e| AdkError::Model(format!("Failed to read response: {}", e)))?;

                let chat_response: ChatCompletionResponse = serde_json::from_str(&response_text)
                    .map_err(|e| AdkError::Model(format!(
                        "Failed to parse response: {} - {}",
                        e, response_text
                    )))?;

                yield convert::from_response(&chat_response);
            }
        };

        Ok(Box::pin(response_stream))
    }
}
