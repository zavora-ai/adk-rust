//! Ollama client implementation.

use super::config::OllamaConfig;
use super::convert;
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, GenericSchemaAdapter, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, Result, SchemaAdapter,
};
use async_stream::try_stream;
use async_trait::async_trait;
use ollama_rs::Ollama;
use ollama_rs::generation::chat::request::ChatMessageRequest;
use ollama_rs::generation::chat::{ChatMessage, ChatMessageResponse};
use ollama_rs::generation::parameters::{FormatType, JsonStructure};
use ollama_rs::generation::tools::{ToolFunctionInfo, ToolInfo, ToolType};
use ollama_rs::models::ModelOptions;
use schemars::Schema;
use serde::Deserialize;

/// Stream that ended, or whose connection failed, before the `done` line.
const INCOMPLETE_STREAM: &str = "model.ollama.incomplete_stream";
/// Stream line that is not valid JSON or not a chat response.
const INVALID_CHUNK: &str = "model.ollama.stream_chunk";
/// Error line Ollama sent inside the stream.
const STREAM_ERROR: &str = "model.ollama.stream_error";

/// Ollama client for local LLM inference.
pub struct OllamaModel {
    client: Ollama,
    /// Sends `/api/chat` requests directly: `ollama-rs` skips error lines and chunks that end
    /// inside a UTF-8 character, does not report a stream that ends before `done`, and drops
    /// every usage count when a response leaves one out.
    http: reqwest::Client,
    model_name: String,
    config: OllamaConfig,
}

impl OllamaModel {
    /// Create a new Ollama client with the given configuration.
    pub fn new(config: OllamaConfig) -> Result<Self> {
        // Parse host URL to extract host and port
        let host = config.host.trim_end_matches('/');
        let client = Ollama::try_new(host).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.ollama.client_init",
                format!("Failed to create Ollama client: {e}"),
            )
            .with_provider("ollama")
        })?;

        let http = ollama_http_client(crate::HttpTimeouts::default())?;

        Ok(Self { client, http, model_name: config.model.clone(), config })
    }

    /// Sets the connect and idle timeouts of the HTTP transport, so a stalled stream fails
    /// with a `Timeout` error instead of holding the run.
    ///
    /// See [`HttpTimeouts`](crate::HttpTimeouts) for the defaults; a local model that takes
    /// longer than the idle timeout to load needs a longer one.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use adk_model::ollama::{OllamaConfig, OllamaModel};
    /// use adk_model::HttpTimeouts;
    ///
    /// let model = OllamaModel::new(OllamaConfig::new("qwen3.5"))?
    ///     .with_timeouts(HttpTimeouts::new(Duration::from_secs(5), Duration::from_secs(900)))?;
    /// # let _ = model;
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `model.ollama.client_init` when the HTTP client cannot be built.
    pub fn with_timeouts(mut self, timeouts: crate::HttpTimeouts) -> Result<Self> {
        self.http = ollama_http_client(timeouts)?;
        Ok(self)
    }

    /// Create a client with default settings for a given model.
    pub fn from_model(model: impl Into<String>) -> Result<Self> {
        Self::new(OllamaConfig::new(model))
    }

    /// Build ModelOptions from config and request.
    fn build_options(&self, request: &LlmRequest) -> ModelOptions {
        let mut options = ModelOptions::default();

        // Apply config options
        if let Some(temp) = self.config.temperature {
            options = options.temperature(temp);
        }
        if let Some(top_p) = self.config.top_p {
            options = options.top_p(top_p);
        }
        if let Some(top_k) = self.config.top_k {
            options = options.top_k(top_k as u32);
        }
        if let Some(num_ctx) = self.config.num_ctx {
            options = options.num_ctx(num_ctx as u64);
        }

        // Apply request config overrides
        if let Some(ref cfg) = request.config {
            if let Some(temp) = cfg.temperature {
                options = options.temperature(temp);
            }
            if let Some(top_p) = cfg.top_p {
                options = options.top_p(top_p);
            }
            if let Some(top_k) = cfg.top_k {
                options = options.top_k(top_k as u32);
            }
            if !cfg.stop_sequences.is_empty() {
                options = options.stop(cfg.stop_sequences.clone());
            }
            if let Some(max_output_tokens) = cfg.max_output_tokens {
                options = options.num_predict(max_output_tokens);
            }
        }

        options
    }

    /// The `/api/chat` body: `chat_request` plus the options `ModelOptions` has no setter for.
    ///
    /// The seed goes in as sent, since Ollama's seed is 64-bit and `ModelOptions::seed` takes
    /// an `i32`.
    fn request_body(
        chat_request: &ChatMessageRequest,
        request: &LlmRequest,
    ) -> Result<serde_json::Value> {
        let mut body = serde_json::to_value(chat_request).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.ollama.request_serialization",
                format!("Failed to serialize Ollama request: {e}"),
            )
            .with_provider("ollama")
        })?;
        if let Some(cfg) = &request.config
            && let Some(options) = body
                .as_object_mut()
                .map(|body| body.entry("options").or_insert_with(|| serde_json::json!({})))
                .and_then(serde_json::Value::as_object_mut)
        {
            let fields = [
                ("seed", cfg.seed.map(serde_json::Value::from)),
                ("presence_penalty", cfg.presence_penalty.map(serde_json::Value::from)),
                ("frequency_penalty", cfg.frequency_penalty.map(serde_json::Value::from)),
            ];
            for (name, value) in fields {
                if let Some(value) = value {
                    options.insert(name.to_string(), value);
                }
            }
        }
        Ok(body)
    }

    /// Convert ADK tool declarations to Ollama tools.
    fn convert_tools(
        &self,
        tools: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Vec<ToolInfo> {
        tools
            .iter()
            .map(|(name, decl)| {
                let description =
                    decl.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parameters_json =
                    decl.get("parameters").cloned().unwrap_or(serde_json::json!({}));
                let parameters: Schema =
                    serde_json::from_value(parameters_json).unwrap_or_else(|_| Schema::from(false));

                ToolInfo {
                    tool_type: ToolType::Function,
                    function: ToolFunctionInfo { name: name.clone(), description, parameters },
                }
            })
            .collect()
    }
}

fn ollama_http_client(timeouts: crate::HttpTimeouts) -> Result<reqwest::Client> {
    timeouts.apply(reqwest::Client::builder()).build().map_err(|e| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.ollama.client_init",
            format!("Failed to create Ollama HTTP client: {e}"),
        )
        .with_provider("ollama")
    })
}

/// Map an Ollama error message to a structured `AdkError`.
///
/// Ollama errors don't carry HTTP status codes directly, so we classify
/// based on message content.
fn ollama_error_to_adk(msg: &str) -> AdkError {
    let upper = msg.to_ascii_uppercase();
    let (category, code) =
        if upper.contains("CONNECTION REFUSED") || upper.contains("CONNECT ERROR") {
            (ErrorCategory::Unavailable, "model.ollama.unavailable")
        } else if upper.contains("TIMEOUT") || upper.contains("TIMED OUT") {
            (ErrorCategory::Timeout, "model.ollama.timeout")
        } else if upper.contains("NOT FOUND") || upper.contains("NO SUCH MODEL") {
            (ErrorCategory::NotFound, "model.ollama.not_found")
        } else {
            (ErrorCategory::Internal, "model.ollama.error")
        };
    // Classified on the whole text; the message quotes a bounded excerpt, since a response
    // body can echo the request.
    AdkError::new(ErrorComponent::Model, category, code, adk_core::upstream_excerpt(msg))
        .with_provider("ollama")
}

/// Map a `serde_json::Value` to an Ollama `FormatType`. This is used for structured JSON output
/// formatting.
fn map_json_value_to_ollama_schema(value: &serde_json::Value) -> Result<FormatType> {
    fn map_convertable_to_format_type(value: &serde_json::Value) -> Result<FormatType> {
        let schema = schemars::Schema::try_from(value.clone()).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.ollama.schema_conversion",
                format!("Failed to convert JSON value to schema: {e}"),
            )
        })?;

        Ok(FormatType::StructuredJson(Box::new(JsonStructure::new_for_schema(schema))))
    }

    // Only `objects` and `bools` can be converted to `scheamrs::Schema` for structured JSON
    // output. Other types are treated as generic JSON.
    let format_type = match value {
        serde_json::Value::Object(_) => map_convertable_to_format_type(value)?,
        serde_json::Value::Bool(_) => map_convertable_to_format_type(value)?,
        _ => FormatType::Json,
    };

    Ok(format_type)
}

#[async_trait]
impl Llm for OllamaModel {
    // Local models bound at zero.
    fn max_cost_micro_usd(&self, request: &adk_core::LlmRequest) -> Option<u64> {
        crate::pricing::PricingCatalog::standard().max_cost_micro_usd(
            Some("ollama"),
            self.name(),
            request,
            crate::pricing::request_output_cap(request),
        )
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    // OllamaModel uses the default GenericSchemaAdapter from the Llm trait.
    // This explicit override documents the intentional adapter selection.
    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        static ADAPTER: GenericSchemaAdapter = GenericSchemaAdapter;
        &ADAPTER
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
    ) -> Result<LlmResponseStream> {
        let usage_span = adk_telemetry::llm_generate_span("ollama", &self.model_name, stream);
        let model = self.model_name.clone();
        crate::generation_config::reject_unsupported(
            "ollama",
            request.config.as_ref(),
            crate::generation_config::Supports {
                stop_sequences: true,
                seed: true,
                penalties: true,
                top_logprobs: true,
            },
        )?;
        let options = self.build_options(&request);

        // Convert ADK contents to Ollama messages
        // A tool message carries only text, so images a tool returned follow it.
        let mut messages: Vec<ChatMessage> = Vec::new();
        for content in crate::tool_result::with_images(&request.contents).iter() {
            if let Some(msg) = convert::content_to_chat_message(content) {
                messages.push(msg);
            }
        }

        // Build chat request
        let mut chat_request = ChatMessageRequest::new(model, messages).options(options);
        let top_logprobs = request.config.as_ref().and_then(|config| config.top_logprobs);
        if let Some(top_logprobs) = top_logprobs {
            chat_request = chat_request.logprobs(true).top_logprobs(u32::from(top_logprobs));
        }

        // Add tools if present
        if !request.tools.is_empty() {
            let tools = self.convert_tools(&request.tools);
            chat_request = chat_request.tools(tools);
        }

        if let Some(config) = &request.config
            && let Some(response_schema) = &config.response_schema
        {
            chat_request = chat_request.format(map_json_value_to_ollama_schema(response_schema)?);
        }

        // Text-encoded tool calls are honoured only for tools this request declared.
        let declared_tools: std::collections::HashSet<String> =
            request.tools.keys().cloned().collect();
        let body = Self::request_body(&chat_request, &request)?;

        // The same endpoint `ollama-rs` derives from the configured host.
        let chat_url = format!("{}api/chat", self.client.url_str());
        let http = self.http.clone();

        let response_stream = try_stream! {
            if stream {
                // Streaming mode — Ollama supports streaming with tool calls since May 2025
                use futures::StreamExt;

                let mut body = body;
                body["stream"] = serde_json::Value::Bool(true);

                let response = http.post(&chat_url).json(&body).send().await.map_err(|e| {
                    let (category, code) = if e.is_timeout() {
                        (ErrorCategory::Timeout, "model.ollama.timeout")
                    } else {
                        (ErrorCategory::Unavailable, "model.ollama.unavailable")
                    };
                    AdkError::new(ErrorComponent::Model, category, code, format!("Ollama stream error: {e}"))
                        .with_provider("ollama")
                })?;
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    let detail = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|value| value.get("error").and_then(serde_json::Value::as_str).map(str::to_owned))
                        .unwrap_or(body);
                    let message = format!(
                        "Ollama stream error: HTTP {status}: {}",
                        adk_core::upstream_excerpt(&detail)
                    );
                    Err(ollama_error_to_adk(&message).with_upstream_status(status.as_u16()))?;
                    return;
                }

                let mut byte_stream = response.bytes_stream();
                // Bytes, not text: a network chunk can end inside a UTF-8 character.
                let mut buffer = Vec::new();
                // Only a `done: true` line marks a complete response.
                let mut done = false;
                // Text-encoded tool calls can span lines, so text passes through the buffer.
                let mut text_tool_buffer = crate::tool_call_parser::ToolCallBuffer::for_declared_tools(
                    declared_tools.clone(),
                );
                let mut ended = false;
                // Each line carries the log probabilities of its own tokens.
                let mut logprobs = Vec::new();

                while !ended {
                    match byte_stream.next().await {
                        Some(chunk) => buffer.extend_from_slice(&chunk.map_err(|error| {
                            crate::stream_error::interrupted("ollama", INCOMPLETE_STREAM, &error)
                        })?),
                        None => {
                            ended = true;
                            // The last line may arrive without its line ending.
                            buffer.push(b'\n');
                        }
                    }

                    while let Some(line) = crate::sse::take_line(&mut buffer)? {
                        if line.is_empty() {
                            continue;
                        }
                        let parsed = serde_json::from_str::<serde_json::Value>(&line);
                        // A cut-off trailer after a finished response loses nothing.
                        if parsed.is_err() && ended && done {
                            continue;
                        }
                        let mut value = parsed.map_err(|error| {
                            crate::stream_error::undecodable("ollama", INVALID_CHUNK, &error)
                        })?;
                        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                            Err(crate::stream_error::reported("ollama", STREAM_ERROR, error))?;
                        }
                        logprobs.extend(convert::take_logprobs(&mut value));
                        // Kept as received: the final line's usage is read from it.
                        let response = ChatMessageResponse::deserialize(&value).map_err(|error| {
                            crate::stream_error::undecodable("ollama", INVALID_CHUNK, &error)
                        })?;

                        // Yield thinking delta for each chunk
                        if let Some(thinking) = &response.message.thinking
                            && !thinking.is_empty() {
                                yield convert::thinking_delta_response(thinking);
                            }

                        // Yield text delta for each chunk
                        if !response.message.content.is_empty()
                            && let crate::tool_call_parser::BufferAction::Emit(parts) =
                                text_tool_buffer.push(&response.message.content)
                        {
                            for part in parts {
                                yield convert::part_delta_response(part);
                            }
                        }

                        // Yield tool calls if present in this chunk. They are part of the
                        // turn the `done` line ends, so they arrive partial: a second terminal
                        // response would be saved as a second event with the same ID.
                        if !response.message.tool_calls.is_empty() {
                            let mut parts = Vec::new();
                            for tool_call in &response.message.tool_calls {
                                parts.push(Part::FunctionCall {
                                    name: tool_call.function.name.clone(),
                                    args: tool_call.function.arguments.clone(),
                                    id: None,
                                    thought_signature: None,
                                });
                            }
                            yield LlmResponse {
                                content: Some(Content { role: "model".to_string(), parts }),
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

                        // If done, yield final response with metadata
                        if response.done {
                            for part in text_tool_buffer.flush() {
                                yield convert::part_delta_response(part);
                            }
                            done = true;
                            let mut last = convert::chat_response_to_llm_response(&response, &value, false, &declared_tools);
                            last.provider_metadata = convert::logprobs_metadata(std::mem::take(&mut logprobs));
                            yield last;
                        }
                    }
                }
                if !done {
                    Err(crate::stream_error::incomplete("ollama", INCOMPLETE_STREAM, "the response finished"))?;
                }
            } else {
                // Non-streaming mode. The body is read as received so that usage reports
                // whichever counts Ollama sent.
                let mut body = body;
                body["stream"] = serde_json::Value::Bool(false);

                let response = http.post(&chat_url).json(&body).send().await.map_err(|e| {
                    let (category, code) = if e.is_timeout() {
                        (ErrorCategory::Timeout, "model.ollama.timeout")
                    } else {
                        (ErrorCategory::Unavailable, "model.ollama.unavailable")
                    };
                    AdkError::new(ErrorComponent::Model, category, code, format!("Ollama error: {e}"))
                        .with_provider("ollama")
                })?;
                let status = response.status();
                let text = response.text().await.map_err(|e| {
                    let (category, code) = if e.is_timeout() {
                        (ErrorCategory::Timeout, "model.ollama.timeout")
                    } else {
                        (ErrorCategory::Unavailable, "model.ollama.unavailable")
                    };
                    AdkError::new(
                        ErrorComponent::Model,
                        category,
                        code,
                        format!("Ollama error: failed to read the response: {e}"),
                    )
                    .with_provider("ollama")
                })?;
                if !status.is_success() {
                    Err(ollama_error_to_adk(&format!("Ollama error: {text}"))
                        .with_upstream_status(status.as_u16()))?;
                }
                let mut line: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                    ollama_error_to_adk(&format!(
                        "Ollama error: invalid response JSON: {}",
                        adk_core::json_error_summary(&e)
                    ))
                })?;
                let logprobs = convert::take_logprobs(&mut line);
                let parsed: ChatMessageResponse = serde_json::from_value(line.clone()).map_err(|e| {
                    ollama_error_to_adk(&format!(
                        "Ollama error: unexpected response shape: {}",
                        adk_core::json_error_summary(&e)
                    ))
                })?;

                let mut response = convert::chat_response_to_llm_response(&parsed, &line, false, &declared_tools);
                response.provider_metadata = convert::logprobs_metadata(logprobs);
                yield response;
            }
        };

        Ok(crate::usage_tracking::with_priced_usage_tracking(
            Box::pin(response_stream),
            usage_span,
            "ollama",
            &self.model_name,
        ))
    }
}
