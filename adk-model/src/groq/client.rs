//! Groq client implementation.

use super::config::{GROQ_API_BASE, GroqConfig};
use super::convert::{self, ChatCompletionRequest, ChatCompletionResponse};
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;

/// Stream that ended, or whose connection failed, before the response finished.
const INCOMPLETE_STREAM: &str = "model.groq.incomplete_stream";
/// Stream chunk that is not valid JSON or not a chat completion chunk.
const INVALID_CHUNK: &str = "model.groq.invalid_chunk";
/// Error Groq reported inside the stream.
const STREAM_ERROR: &str = "model.groq.stream_error";

/// Groq client for ultra-fast LLM inference.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::groq::{GroqClient, GroqConfig};
///
/// let client = GroqClient::new(GroqConfig::gpt_oss_120b(
///     std::env::var("GROQ_API_KEY").unwrap()
/// ))?;
/// ```
pub struct GroqClient {
    client: Client,
    config: GroqConfig,
    retry_config: RetryConfig,
}

impl GroqClient {
    /// Create a new Groq client.
    pub fn new(config: GroqConfig) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("groq", &config.model);
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;

        Ok(Self { client, config, retry_config: RetryConfig::default() })
    }

    /// Sets the connect and idle timeouts of the HTTP transport, so a stalled stream fails
    /// with a `Timeout` error instead of holding the run.
    ///
    /// See [`HttpTimeouts`](crate::HttpTimeouts) for the defaults.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use adk_model::groq::{GroqClient, GroqConfig};
    /// use adk_model::HttpTimeouts;
    ///
    /// let client = GroqClient::new(GroqConfig::new("gsk-key", "openai/gpt-oss-120b"))?
    ///     .with_timeouts(HttpTimeouts::new(Duration::from_secs(10), Duration::from_secs(120)))?;
    /// # let _ = client;
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the Groq HTTP client cannot be built.
    pub fn with_timeouts(mut self, timeouts: crate::HttpTimeouts) -> Result<Self, AdkError> {
        self.client = timeouts
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        Ok(self)
    }

    /// Create a client for the legacy `llama-3.3-70b-versatile` model.
    ///
    /// See [`GroqConfig::llama70b`] for its availability.
    pub fn llama70b(api_key: impl Into<String>) -> Result<Self, AdkError> {
        Self::new(GroqConfig::llama70b(api_key))
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

    /// Build the API URL for chat completions.
    fn api_url(&self) -> String {
        let base = self.config.base_url.as_deref().unwrap_or(GROQ_API_BASE);
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    /// Build a chat completion request from an LLM request.
    ///
    /// # Errors
    ///
    /// Returns `model.unsupported_config` when the config sets `top_logprobs`, which Groq
    /// models do not support.
    fn build_request(
        &self,
        request: &LlmRequest,
        stream: bool,
    ) -> Result<ChatCompletionRequest, AdkError> {
        let config = request.config.as_ref();
        crate::generation_config::reject_unsupported(
            "groq",
            config,
            crate::generation_config::Supports {
                stop_sequences: true,
                seed: true,
                penalties: true,
                top_logprobs: false,
            },
        )?;
        // A tool message carries only text, so images a tool returned follow it.
        let messages: Vec<_> = crate::tool_result::with_images(&request.contents)
            .iter()
            .flat_map(convert::content_to_messages)
            .collect();

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(convert::convert_tools(&request.tools))
        };

        let temperature = request.config.as_ref().and_then(|c| c.temperature);
        let top_p = request.config.as_ref().and_then(|c| c.top_p);
        let max_tokens = request
            .config
            .as_ref()
            .and_then(|c| c.max_output_tokens)
            .map(|t| t as u32)
            .or(self.config.max_tokens);

        let include_reasoning = if self.config.reasoning_enabled { Some(true) } else { None };

        Ok(ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature,
            top_p,
            max_tokens,
            stream: Some(stream),
            tools,
            include_reasoning,
            stop: config.map(|c| c.stop_sequences.clone()).filter(|stop| !stop.is_empty()),
            seed: config.and_then(|c| c.seed),
            frequency_penalty: config.and_then(|c| c.frequency_penalty),
            presence_penalty: config.and_then(|c| c.presence_penalty),
        })
    }
}

#[async_trait]
impl Llm for GroqClient {
    // Groq does not state whether reasoning counts toward `max_completion_tokens`, so the model's output limit bounds every request.
    fn max_cost_micro_usd(&self, request: &adk_core::LlmRequest) -> Option<u64> {
        crate::pricing::PricingCatalog::standard().max_cost_micro_usd(
            Some("groq"),
            self.name(),
            request,
            None,
        )
    }

    fn name(&self) -> &str {
        &self.config.model
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
        let usage_span = adk_telemetry::llm_generate_span("groq", &self.config.model, stream);
        let api_url = self.api_url();
        let api_key = self.config.api_key.clone();
        let chat_request = self.build_request(&request, stream)?;
        let client = self.client.clone();
        let retry_config = self.retry_config.clone();

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
                        .map_err(|e| AdkError::new(
                            ErrorComponent::Model,
                            crate::http_timeouts::transport_category(&e),
                            "model.groq.request",
                            format!("Groq API request failed: {e}"),
                        ).with_provider("groq"))?;

                    if !response.status().is_success() {
                        let status = response.status();
                        let status_code = status.as_u16();
                        let retry_after = response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|value| value.to_str().ok())
                            .and_then(crate::retry::parse_retry_after);
                        let error_text = response.text().await.unwrap_or_default();
                        let error = AdkError::new(
                            ErrorComponent::Model,
                            crate::retry::category_for_status_code(status_code),
                            "model.groq.api_error",
                            format!(
                                "Groq API error (HTTP {status}): {}",
                                adk_core::upstream_excerpt(&error_text)
                            ),
                        ).with_upstream_status(status_code).with_provider("groq");
                        return Err(crate::retry::with_retry_after(error, retry_after));
                    }

                    Ok(response)
                }
            })
            .await?;

            if stream {
                // Streaming mode - process SSE events
                let mut byte_stream = response.bytes_stream();
                // Bytes, not text: a network chunk can end inside a UTF-8 character.
                let mut buffer = Vec::new();
                // A finish reason or `[DONE]` marks a complete response; without one the
                // answer or a tool call may be truncated.
                let mut finished = false;
                let mut ended = false;

                // Accumulate tool calls across chunks
                let mut tool_call_accumulators: std::collections::HashMap<u32, (String, String, String)> =
                    std::collections::HashMap::new();

                while !ended {
                    match byte_stream.next().await {
                        Some(chunk) => buffer.extend_from_slice(&chunk.map_err(|error| {
                            crate::stream_error::interrupted("groq", INCOMPLETE_STREAM, &error)
                        })?),
                        None => {
                            ended = true;
                            // The last line may arrive without its line ending.
                            buffer.push(b'\n');
                        }
                    }

                    // Process complete SSE events
                    while let Some(line) = crate::sse::take_line(&mut buffer)? {
                        let Some(data) = crate::sse::data(&line) else {
                            continue;
                        };

                        if data == "[DONE]" {
                            if !tool_call_accumulators.is_empty() {
                                Err(crate::stream_error::incomplete(
                                    "groq",
                                    INCOMPLETE_STREAM,
                                    "its tool calls finished",
                                ))?;
                            }
                            finished = true;
                            continue;
                        }

                        let parsed = serde_json::from_str::<serde_json::Value>(data);
                        // A cut-off trailer after a finished response loses nothing.
                        if parsed.is_err() && ended && finished {
                            continue;
                        }
                        let value = parsed.map_err(|error| {
                            crate::stream_error::undecodable("groq", INVALID_CHUNK, &error)
                        })?;
                        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                            Err(crate::stream_error::reported("groq", STREAM_ERROR, error))?;
                        }
                        // A chunk that does not decode can be the one finishing the turn,
                        // so it fails the stream rather than dropping its tool calls.
                        let chunk_response: ChatCompletionResponse = serde_json::from_value(value)
                            .map_err(|error| {
                                crate::stream_error::undecodable("groq", INVALID_CHUNK, &error)
                            })?;
                        if let Some(choice) = chunk_response.choices.first() {
                            // Handle tool call accumulation
                            if let Some(delta) = &choice.delta
                                && let Some(tool_calls) = &delta.tool_calls {
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

                            // Check for finish
                            if let Some(reason) = choice.finish_reason.as_deref() {
                                if reason == "error" {
                                    Err(crate::stream_error::reported(
                                        "groq",
                                        STREAM_ERROR,
                                        &serde_json::Value::Null,
                                    ))?;
                                }
                                finished = true;
                                let finish_reason = Some(match reason {
                                    "stop" | "tool_calls" => FinishReason::Stop,
                                    "length" => FinishReason::MaxTokens,
                                    "content_filter" => FinishReason::Safety,
                                    _ => FinishReason::Other,
                                });

                                // Emit tool calls if any
                                if !tool_call_accumulators.is_empty() {
                                    let mut sorted_calls: Vec<_> =
                                        tool_call_accumulators.drain().collect();
                                    sorted_calls.sort_by_key(|(idx, _)| *idx);

                                    let tool_calls = sorted_calls
                                        .into_iter()
                                        .map(|(_, (id, name, args_str))| {
                                            let args = crate::tool_args::streamed_tool_arguments(
                                                "groq",
                                                &name,
                                                &args_str,
                                            );
                                            (id, name, args)
                                        })
                                        .collect::<Vec<_>>();

                                    let mut response = convert::create_tool_call_response(
                                        tool_calls,
                                        finish_reason,
                                    );
                                    response.usage_metadata = chunk_response.usage_metadata();
                                    yield response;
                                    continue;
                                }

                                // Emit final response
                                let mut parts = Vec::new();

                                if let Some(delta) = &choice.delta
                                    && let Some(text) = &delta.content
                                        && !text.is_empty() {
                                            parts.push(Part::Text { text: text.clone() });
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
                                    usage_metadata: chunk_response.usage_metadata(),
                                    finish_reason,
                                    citation_metadata: None,
                                    partial: false,
                                    turn_complete: true,
                                    interrupted: false,
                                    error_code: None,
                                    error_message: None,
                                    provider_metadata: None,
                                    interaction_id: None,
                                    model: None,
                                    provider: None,
                                };
                            } else {
                                // Emit partial text content
                                if let Some(delta) = &choice.delta
                                    && let Some(text) = &delta.content
                                        && !text.is_empty() {
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
                                                provider_metadata: None,
                                                interaction_id: None,
                                                model: None,
                                                provider: None,
                                            };
                                        }
                            }
                        }
                    }
                }
                if !finished {
                    Err(crate::stream_error::incomplete(
                        "groq",
                        INCOMPLETE_STREAM,
                        if tool_call_accumulators.is_empty() {
                            "the response finished"
                        } else {
                            "its tool calls finished"
                        },
                    ))?;
                }
            } else {
                // Non-streaming mode
                let response_text = response.text().await.map_err(|e| AdkError::new(
                    ErrorComponent::Model,
                    crate::http_timeouts::transport_category(&e),
                    "model.groq.request",
                    format!("Failed to read response: {e}"),
                ).with_provider("groq"))?;

                let chat_response: ChatCompletionResponse = serde_json::from_str(&response_text)
                    .map_err(|e| AdkError::model(format!(
                        "Failed to parse response: {}",
                        adk_core::json_error_summary(&e)
                    )))?;

                yield convert::from_response(&chat_response);
            }
        };

        Ok(crate::usage_tracking::with_priced_usage_tracking(
            Box::pin(response_stream),
            usage_span,
            "groq",
            &self.config.model,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, GenerateContentConfig};

    fn request(config: GenerateContentConfig) -> LlmRequest {
        let mut request =
            LlmRequest::new("qwen/qwen3.8-27b", vec![Content::new("user").with_text("hi")]);
        request.config = Some(config);
        request
    }

    #[test]
    fn stop_seed_and_penalties_are_sent_and_logprobs_are_rejected() {
        let client = GroqClient::new(GroqConfig::new("test-key", "qwen/qwen3.8-27b")).unwrap();
        let built = client
            .build_request(
                &request(GenerateContentConfig {
                    stop_sequences: vec!["END".to_string()],
                    seed: Some(42),
                    presence_penalty: Some(0.5),
                    frequency_penalty: Some(0.25),
                    ..Default::default()
                }),
                false,
            )
            .unwrap();
        let wire = serde_json::to_value(built).unwrap();
        assert_eq!(wire["stop"], serde_json::json!(["END"]));
        assert_eq!(wire["seed"], 42);
        assert_eq!(wire["presence_penalty"], 0.5);
        assert_eq!(wire["frequency_penalty"], 0.25);

        let error = client
            .build_request(
                &request(GenerateContentConfig { top_logprobs: Some(2), ..Default::default() }),
                false,
            )
            .expect_err("Groq models do not return log probabilities");
        assert_eq!(error.code, "model.unsupported_config");
    }
}
