//! Anthropic client implementation.

use super::config::AnthropicConfig;
use super::convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, FinishReason, Llm, LlmRequest, Part};
use async_stream::try_stream;
use async_trait::async_trait;
use claudius::{
    Anthropic, ContentBlock, ContentBlockDelta, ContentBlockDeltaEvent, MessageStreamEvent,
    StopReason, TextDelta,
};
use futures::StreamExt;
use std::pin::pin;

/// Anthropic client for Claude models.
pub struct AnthropicClient {
    client: Anthropic,
    model: String,
    max_tokens: u32,
    retry_config: RetryConfig,
}

impl AnthropicClient {
    /// Create a new Anthropic client.
    pub fn new(config: AnthropicConfig) -> Result<Self, AdkError> {
        let client = Anthropic::new(Some(config.api_key.clone()))
            .map_err(|e| AdkError::Model(format!("Failed to create Anthropic client: {}", e)))?;

        Ok(Self {
            client,
            model: config.model,
            max_tokens: config.max_tokens,
            retry_config: RetryConfig::default(),
        })
    }

    /// Create a client with just an API key (uses default model).
    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(AnthropicConfig::new(api_key, "claude-sonnet-4-20250514"))
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

    fn build_message_params(
        model: &str,
        max_tokens: u32,
        request: &LlmRequest,
    ) -> claudius::MessageCreateParams {
        let mut system_prompt = None;
        let mut messages = Vec::new();

        for content in &request.contents {
            if content.role == "system" {
                let text: String = content
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    system_prompt = Some(text);
                }
            } else {
                messages.push(convert::content_to_message(content));
            }
        }

        let tools = if request.tools.is_empty() {
            Vec::new()
        } else {
            convert::convert_tools(&request.tools)
        };

        let temperature = request.config.as_ref().and_then(|c| c.temperature);
        let top_p = request.config.as_ref().and_then(|c| c.top_p);
        let top_k = request.config.as_ref().and_then(|c| c.top_k);
        let effective_max_tokens = request
            .config
            .as_ref()
            .and_then(|c| c.max_output_tokens)
            .map(|t| t as u32)
            .unwrap_or(max_tokens);

        convert::build_message_params(
            model,
            effective_max_tokens,
            messages,
            tools,
            system_prompt,
            temperature,
            top_p,
            top_k,
        )
    }
}

#[async_trait]
impl Llm for AnthropicClient {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<adk_core::LlmResponseStream, AdkError> {
        let model = self.model.clone();
        let max_tokens = self.max_tokens;
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();

        let response_stream = try_stream! {
            if stream {
                // Streaming mode
                let client_ref = &client;
                let model_ref = model.as_str();
                let event_stream = execute_with_retry(&retry_config, is_retryable_model_error, || {
                    let request = request_for_retry.clone();
                    async move {
                        let params = Self::build_message_params(model_ref, max_tokens, &request);
                        client_ref
                            .stream(params)
                            .await
                            .map_err(|e| AdkError::Model(format!("Anthropic API error: {}", e)))
                    }
                })
                .await?;

                // Pin the stream for iteration
                let mut pinned_stream = pin!(event_stream);

                // Track tool calls being built
                let mut current_tool_calls: Vec<(String, String, String)> = Vec::new(); // (id, name, args_json)
                let mut current_tool_index: Option<usize> = None;

                while let Some(event_result) = pinned_stream.next().await {
                    let event = event_result
                        .map_err(|e| AdkError::Model(format!("Stream error: {}", e)))?;

                    match event {
                        MessageStreamEvent::ContentBlockStart(start_event) => {
                            // Check if this is a tool_use block
                            let index = start_event.index;
                            if let ContentBlock::ToolUse(tool_use) = start_event.content_block {
                                current_tool_index = Some(index);
                                // Ensure vector is large enough
                                while current_tool_calls.len() <= index {
                                    current_tool_calls.push((String::new(), String::new(), String::new()));
                                }
                                current_tool_calls[index] = (
                                    tool_use.id.clone(),
                                    tool_use.name.clone(),
                                    String::new(),
                                );
                            }
                        }
                        MessageStreamEvent::ContentBlockDelta(ContentBlockDeltaEvent { index, delta }) => {
                            match delta {
                                ContentBlockDelta::TextDelta(TextDelta { text }) => {
                                    if !text.is_empty() {
                                        yield convert::from_text_delta(&text);
                                    }
                                }
                                ContentBlockDelta::InputJsonDelta(json_delta) => {
                                    // Accumulate tool call arguments
                                    if let Some(idx) = current_tool_index {
                                        if idx < current_tool_calls.len() {
                                            current_tool_calls[idx].2.push_str(&json_delta.partial_json);
                                        }
                                    } else if index < current_tool_calls.len() {
                                        current_tool_calls[index].2.push_str(&json_delta.partial_json);
                                    }
                                }
                                _ => {}
                            }
                        }
                        MessageStreamEvent::ContentBlockStop { .. } => {
                            current_tool_index = None;
                        }
                        MessageStreamEvent::MessageDelta(delta_event) => {
                            // Check for stop reason
                            if let Some(stop_reason) = &delta_event.delta.stop_reason {
                                let finish_reason = match stop_reason {
                                    StopReason::EndTurn => Some(FinishReason::Stop),
                                    StopReason::MaxTokens => Some(FinishReason::MaxTokens),
                                    StopReason::StopSequence => Some(FinishReason::Stop),
                                    StopReason::ToolUse => Some(FinishReason::Stop),
                                    _ => Some(FinishReason::Stop),
                                };

                                // If we have accumulated tool calls, emit them
                                if !current_tool_calls.is_empty() {
                                    let tool_calls: Vec<_> = current_tool_calls
                                        .drain(..)
                                        .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
                                        .map(|(id, name, args_str)| {
                                            let args: serde_json::Value = serde_json::from_str(&args_str)
                                                .unwrap_or(serde_json::json!({}));
                                            (id, name, args)
                                        })
                                        .collect();

                                    if !tool_calls.is_empty() {
                                        yield convert::create_tool_call_response(tool_calls, finish_reason);
                                        continue;
                                    }
                                }

                                // Emit final message
                                yield adk_core::LlmResponse {
                                    content: None,
                                    usage_metadata: Some(adk_core::UsageMetadata {
                                        prompt_token_count: 0,
                                        candidates_token_count: delta_event.usage.output_tokens,
                                        total_token_count: delta_event.usage.output_tokens,
                                    }),
                                    finish_reason,
                                    partial: false,
                                    turn_complete: true,
                                    interrupted: false,
                                    error_code: None,
                                    error_message: None,
                                };
                            }
                        }
                        MessageStreamEvent::MessageStop(_) => {
                            // Stream complete
                        }
                        _ => {}
                    }
                }
            } else {
                // Non-streaming mode
                let client_ref = &client;
                let model_ref = model.as_str();
                let message = execute_with_retry(&retry_config, is_retryable_model_error, || {
                    let request = request_for_retry.clone();
                    async move {
                        let params = Self::build_message_params(model_ref, max_tokens, &request);
                        client_ref
                            .send(params)
                            .await
                            .map_err(|e| AdkError::Model(format!("Anthropic API error: {}", e)))
                    }
                })
                .await?;

                yield convert::from_anthropic_message(&message);
            }
        };

        Ok(Box::pin(response_stream))
    }
}
