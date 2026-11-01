//! Muse client implementation.

use super::config::{MUSE_API_BASE, MuseApi, MuseConfig};
use super::convert::{self, ChatCompletionRequest, ChatCompletionResponse};
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

/// Strict structured-output format for Muse, named after the model (Meta JSON
/// Schema mode guarantees schema-conformant JSON). Mirrors the OpenAI pattern.
fn response_format_for(model: &str, schema: &Value) -> convert::ResponseFormat {
    /// Strict mode requires `additionalProperties: false` on every object schema.
    fn close_objects(value: &mut Value) {
        match value {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("object") {
                    object
                        .entry("additionalProperties".to_string())
                        .or_insert(serde_json::json!(false));
                }
                for child in object.values_mut() {
                    close_objects(child);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(close_objects),
            _ => {}
        }
    }
    let mut schema = schema.clone();
    close_objects(&mut schema);
    let mut name: String = model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    if name.is_empty() {
        name.push_str("response");
    }
    convert::ResponseFormat {
        format_type: "json_schema".to_string(),
        json_schema: convert::JsonSchema { name, schema, strict: true },
    }
}

/// Stream that ended, or whose connection failed, before the response finished.
const INCOMPLETE_STREAM: &str = "model.muse.incomplete_stream";
/// Stream chunk that is not valid JSON or not a chat completion chunk.
const INVALID_CHUNK: &str = "model.muse.invalid_chunk";
/// Error Muse reported inside the stream.
const STREAM_ERROR: &str = "model.muse.stream_error";

/// Muse client for Meta's Muse Spark models.
///
/// Calls Chat Completions by default, or the Responses API with
/// [`MuseConfig::with_api`]`(`[`MuseApi::Responses`]`)` (feature `muse-responses`).
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseClient, MuseConfig};
///
/// let client = MuseClient::new(MuseConfig::spark_1_3(
///     std::env::var("META_API_KEY").unwrap()
/// ))?;
/// ```
pub struct MuseClient {
    client: Client,
    config: MuseConfig,
    retry_config: RetryConfig,
    /// The Open Responses engine, when the config selects [`MuseApi::Responses`].
    #[cfg(feature = "muse-responses")]
    responses: Option<crate::openai::OpenAIResponsesClient>,
}

impl MuseClient {
    /// Create a new Muse client.
    ///
    /// # Errors
    ///
    /// Returns `model.muse.invalid_config` when the config selects [`MuseApi::Responses`]
    /// without the `muse-responses` feature, or an error when an HTTP client cannot be
    /// built.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_model::muse::{MuseApi, MuseClient, MuseConfig};
    ///
    /// let chat = MuseClient::new(MuseConfig::spark_1_3("LLM-key"))?;
    /// assert_eq!(chat.api(), MuseApi::ChatCompletions);
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    pub fn new(config: MuseConfig) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("muse", &config.model);
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        #[cfg(feature = "muse-responses")]
        let responses = match config.api {
            MuseApi::ChatCompletions => None,
            MuseApi::Responses => Some(
                crate::openai::OpenAIResponsesClient::new(
                    crate::openai::OpenAIResponsesConfig::new(
                        config.api_key.clone(),
                        config.model.clone(),
                    )
                    .with_base_url(config.effective_base_url().trim_end_matches('/'))
                    .with_open_responses_mode(true),
                )?
                .with_provider_name("muse"),
            ),
        };
        #[cfg(not(feature = "muse-responses"))]
        if config.api == MuseApi::Responses {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.muse.invalid_config",
                "MuseApi::Responses needs the adk-model `muse-responses` feature",
            )
            .with_provider("muse"));
        }

        Ok(Self {
            client,
            config,
            retry_config: RetryConfig::default(),
            #[cfg(feature = "muse-responses")]
            responses,
        })
    }

    /// The endpoint the client calls.
    pub fn api(&self) -> MuseApi {
        self.config.api
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
    /// use adk_model::muse::{MuseClient, MuseConfig};
    /// use adk_model::HttpTimeouts;
    ///
    /// let client = MuseClient::new(MuseConfig::new("LLM-key", "muse-spark-1.3"))?
    ///     .with_timeouts(HttpTimeouts::new(Duration::from_secs(10), Duration::from_secs(120)))?;
    /// # let _ = client;
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the Muse HTTP client cannot be built.
    pub fn with_timeouts(mut self, timeouts: crate::HttpTimeouts) -> Result<Self, AdkError> {
        self.client = timeouts
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        #[cfg(feature = "muse-responses")]
        {
            self.responses =
                self.responses.map(|responses| responses.with_timeouts(timeouts)).transpose()?;
        }
        Ok(self)
    }

    /// Set the retry configuration (builder pattern).
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.set_retry_config(retry_config);
        self
    }

    /// Set the retry configuration (mutable reference).
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        #[cfg(feature = "muse-responses")]
        if let Some(responses) = self.responses.as_mut() {
            responses.set_retry_config(retry_config.clone());
        }
        self.retry_config = retry_config;
    }

    /// Returns the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    /// Build the API URL for chat completions.
    fn api_url(&self) -> String {
        let base = self.config.base_url.as_deref().unwrap_or(MUSE_API_BASE);
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    /// The request the Responses engine sends: the Muse request profile checked, the
    /// `extensions["muse"]` options moved to the engine's namespace, and the output cap
    /// raised to [`MUSE_MIN_MAX_TOKENS`](super::config::MUSE_MIN_MAX_TOKENS).
    ///
    /// # Errors
    ///
    /// Returns `model.unsupported_config` for a setting the Meta Responses API rejects.
    #[cfg(feature = "muse-responses")]
    fn responses_request(
        &self,
        mut request: LlmRequest,
        stream: bool,
    ) -> Result<LlmRequest, AdkError> {
        crate::generation_config::reject_unsupported(
            "muse",
            request.config.as_ref(),
            crate::generation_config::Supports {
                stop_sequences: false,
                seed: false,
                penalties: false,
                top_logprobs: false,
            },
        )?;
        let unsupported = |message: &str| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.unsupported_config",
                message,
            )
            .with_provider("muse")
        };
        let config = request.config.get_or_insert_with(Default::default);
        let options = config.extensions.remove("muse");
        let option = |key: &str| options.as_ref().and_then(|options| options.get(key));
        if stream && option("background").and_then(Value::as_bool) == Some(true) {
            return Err(unsupported(
                "the Meta Responses API rejects `background` with streaming; send the background request without streaming and poll it",
            ));
        }
        if option("reasoning").and_then(|reasoning| reasoning.get("effort")).and_then(Value::as_str)
            == Some("none")
        {
            return Err(unsupported(
                "the Meta Responses API rejects reasoning effort `none`; use `low`",
            ));
        }
        let encrypted = option("include").and_then(Value::as_array).is_some_and(|include| {
            include.iter().any(|item| item.as_str() == Some("reasoning.encrypted_content"))
        });
        if encrypted && option("previous_response_id").is_some() {
            return Err(unsupported(
                "the Meta Responses API rejects `reasoning.encrypted_content` with `previous_response_id`; chain turns with `previous_response_id` alone",
            ));
        }
        // The engine reads `extensions["openai"]`; an OpenAI namespace set for another client
        // is not sent to Meta.
        let forwarded: serde_json::Map<String, Value> =
            ["previous_response_id", "store", "reasoning", "include", "background"]
                .into_iter()
                .filter_map(|key| option(key).map(|value| (key.to_string(), value.clone())))
                .collect();
        config.extensions.insert("openai".to_string(), Value::Object(forwarded));
        let cap = config
            .max_output_tokens
            .and_then(|tokens| u32::try_from(tokens).ok())
            .or(self.config.max_tokens);
        config.max_output_tokens = cap
            .map(|cap| cap.max(super::config::MUSE_MIN_MAX_TOKENS))
            .and_then(|cap| i32::try_from(cap).ok());
        Ok(request)
    }

    /// Build a chat completion request from an LLM request.
    ///
    /// # Errors
    ///
    /// Returns `model.unsupported_config` when the config sets `top_logprobs`, which the
    /// Meta Model API does not document for Chat Completions.
    fn build_request(
        &self,
        request: &LlmRequest,
        stream: bool,
    ) -> Result<ChatCompletionRequest, AdkError> {
        let config = request.config.as_ref();
        crate::generation_config::reject_unsupported(
            "muse",
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
            .or(self.config.max_tokens)
            .map(|cap| cap.max(super::config::MUSE_MIN_MAX_TOKENS));
        let reasoning_effort =
            config.and_then(|c| c.effort).map(|level| level.as_str().to_string());
        let options = config.and_then(|c| c.extensions.get("muse"));
        let response_format = config
            .and_then(|c| c.response_schema.as_ref())
            .map(|schema| response_format_for(&self.config.model, schema));

        Ok(ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            temperature,
            top_p,
            max_tokens,
            stream: Some(stream),
            tools,
            stop: config.map(|c| c.stop_sequences.clone()).filter(|stop| !stop.is_empty()),
            seed: config.and_then(|c| c.seed),
            frequency_penalty: config.and_then(|c| c.frequency_penalty),
            presence_penalty: config.and_then(|c| c.presence_penalty),
            reasoning_effort,
            response_format,
            tool_choice: options.and_then(|options| options.get("tool_choice").cloned()),
            parallel_tool_calls: options
                .and_then(|options| options.get("parallel_tool_calls"))
                .and_then(Value::as_bool),
            prompt_cache_key: options
                .and_then(|options| options.get("prompt_cache_key"))
                .and_then(Value::as_str)
                .map(str::to_string),
            prompt_cache_retention: options
                .and_then(|options| options.get("prompt_cache_retention"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

#[async_trait]
impl Llm for MuseClient {
    fn max_cost_micro_usd(&self, request: &adk_core::LlmRequest) -> Option<u64> {
        crate::pricing::PricingCatalog::standard().max_cost_micro_usd(
            Some("muse"),
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
        mut request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        // Native items reach this provider only from the turns its own origin marks.
        adk_core::history::retain_replayable(
            &mut request.contents,
            &adk_core::history::ProviderOrigin::new("muse", &self.config.model),
        );
        #[cfg(feature = "muse-responses")]
        if let Some(responses) = &self.responses {
            let request = self.responses_request(request, stream)?;
            return responses.generate_content(request, stream).await;
        }
        let usage_span = adk_telemetry::llm_generate_span("muse", &self.config.model, stream);
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
                        .header("Authorization", format!("Bearer {api_key}"))
                        .header("Content-Type", "application/json")
                        .json(&chat_request)
                        .send()
                        .await
                        .map_err(|e| AdkError::new(
                            ErrorComponent::Model,
                            crate::http_timeouts::transport_category(&e),
                            "model.muse.request",
                            format!("Muse API request failed: {e}"),
                        ).with_provider("muse"))?;

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
                            "model.muse.api_error",
                            format!(
                                "Muse API error (HTTP {status}): {}",
                                adk_core::upstream_excerpt(&error_text)
                            ),
                        ).with_upstream_status(status_code).with_provider("muse");
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
                            crate::stream_error::interrupted("muse", INCOMPLETE_STREAM, &error)
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
                                    "muse",
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
                            crate::stream_error::undecodable("muse", INVALID_CHUNK, &error)
                        })?;
                        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                            Err(crate::stream_error::reported("muse", STREAM_ERROR, error))?;
                        }
                        // A chunk that does not decode can be the one finishing the turn,
                        // so it fails the stream rather than dropping its tool calls.
                        let chunk_response: ChatCompletionResponse = serde_json::from_value(value)
                            .map_err(|error| {
                                crate::stream_error::undecodable("muse", INVALID_CHUNK, &error)
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
                                                    format!("call_{index}")
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
                                        "muse",
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
                                                "muse",
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
                        "muse",
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
                    "model.muse.request",
                    format!("Failed to read response: {e}"),
                ).with_provider("muse"))?;

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
            "muse",
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
            LlmRequest::new("muse-spark-1.3", vec![Content::new("user").with_text("hi")]);
        request.config = Some(config);
        request
    }

    #[test]
    fn stop_seed_and_penalties_are_sent_and_logprobs_are_rejected() {
        let client = MuseClient::new(MuseConfig::new("test-key", "muse-spark-1.3")).unwrap();
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
            .expect_err("Muse Chat Completions does not document log probabilities");
        assert_eq!(error.code, "model.unsupported_config");
    }

    #[test]
    fn small_caps_are_raised_to_the_reasoning_floor() {
        use adk_core::Effort;

        let client = MuseClient::new(MuseConfig::new("test-key", "muse-spark-1.3")).unwrap();
        let tiny = client
            .build_request(
                &request(GenerateContentConfig {
                    max_output_tokens: Some(256),
                    ..Default::default()
                }),
                false,
            )
            .unwrap();
        assert_eq!(tiny.max_tokens, Some(crate::muse::config::MUSE_MIN_MAX_TOKENS));

        let roomy = client
            .build_request(
                &request(GenerateContentConfig {
                    max_output_tokens: Some(4096),
                    ..Default::default()
                }),
                false,
            )
            .unwrap();
        assert_eq!(roomy.max_tokens, Some(4096));

        let effort = client
            .build_request(
                &request(GenerateContentConfig { effort: Some(Effort::Low), ..Default::default() }),
                false,
            )
            .unwrap();
        assert_eq!(effort.reasoning_effort.as_deref(), Some("low"));

        let no_effort = client.build_request(&request(Default::default()), false).unwrap();
        assert_eq!(no_effort.reasoning_effort, None);
        assert_eq!(no_effort.max_tokens, None);
    }

    #[test]
    fn response_schema_and_options_are_sent() {
        let client = MuseClient::new(MuseConfig::new("test-key", "muse-spark-1.3")).unwrap();
        let mut config = GenerateContentConfig {
            max_output_tokens: Some(256),
            response_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "card": {"type": "object", "properties": {"x": {"type": "string"}}},
                },
                "required": ["title"],
            })),
            ..Default::default()
        };
        config.extensions.insert(
            "muse".to_string(),
            serde_json::json!({
                "prompt_cache_key": "ideate",
                "prompt_cache_retention": "24h",
                "tool_choice": "required",
                "parallel_tool_calls": false,
            }),
        );
        let built = client.build_request(&request(config), false).unwrap();
        let wire = serde_json::to_value(built).unwrap();
        assert_eq!(wire["response_format"]["type"], "json_schema");
        assert_eq!(wire["response_format"]["json_schema"]["strict"], true);
        assert_eq!(wire["response_format"]["json_schema"]["schema"]["additionalProperties"], false);
        assert_eq!(
            wire["response_format"]["json_schema"]["schema"]["properties"]["card"]["additionalProperties"],
            false
        );
        assert_eq!(wire["prompt_cache_key"], "ideate");
        assert_eq!(wire["prompt_cache_retention"], "24h");
        assert_eq!(wire["tool_choice"], "required");
        assert_eq!(wire["parallel_tool_calls"], false);
        assert_eq!(wire["max_completion_tokens"], 1024);
        assert!(wire.get("max_tokens").is_none());
    }

    #[test]
    fn chat_url_appends_completions_to_base() {
        let client = MuseClient::new(MuseConfig::spark_1_3("test-key")).unwrap();
        assert_eq!(client.api_url(), "https://api.meta.ai/v1/chat/completions");

        let custom = MuseClient::new(
            MuseConfig::spark_1_3("test-key").with_base_url("https://proxy.local/meta/"),
        )
        .unwrap();
        assert_eq!(custom.api_url(), "https://proxy.local/meta/chat/completions");
    }
}
