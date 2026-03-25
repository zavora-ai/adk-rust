//! OpenAI Responses API client implementation.

use super::config::{OpenAIResponsesConfig, ReasoningEffort, ReasoningSummary};
use super::responses_convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;

/// Client for the OpenAI Responses API (`/responses` endpoint).
///
/// Wraps `async-openai`'s typed `Responses` client and implements `adk_core::Llm`.
/// Supports reasoning summaries, conversation state via `previous_response_id`,
/// and built-in tools (web search, file search, code interpreter).
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
///
/// let config = OpenAIResponsesConfig::new("sk-...", "o3");
/// let client = OpenAIResponsesClient::new(config)?;
/// ```
pub struct OpenAIResponsesClient {
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_summary: Option<ReasoningSummary>,
    retry_config: RetryConfig,
}

impl OpenAIResponsesClient {
    /// Create a new Responses API client from the given config.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with `InvalidInput` if `api_key` is empty.
    pub fn new(config: OpenAIResponsesConfig) -> Result<Self, AdkError> {
        if config.api_key.is_empty() {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openai_responses.invalid_config",
                "OpenAI Responses API key must not be empty",
            )
            .with_provider("openai-responses"));
        }

        let mut openai_config =
            async_openai::config::OpenAIConfig::new().with_api_key(&config.api_key);
        if let Some(org_id) = &config.organization_id {
            openai_config = openai_config.with_org_id(org_id);
        }
        if let Some(base_url) = &config.base_url {
            openai_config = openai_config.with_api_base(base_url);
        }
        let client = async_openai::Client::with_config(openai_config);

        Ok(Self {
            client,
            model: config.model,
            reasoning_effort: config.reasoning_effort,
            reasoning_summary: config.reasoning_summary,
            retry_config: RetryConfig::default(),
        })
    }

    /// Set the retry configuration, consuming self.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set the retry configuration by mutable reference.
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Get a reference to the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}

/// Map an `async_openai::error::OpenAIError` to an `AdkError`.
fn map_openai_error(e: async_openai::error::OpenAIError) -> AdkError {
    let error_string = e.to_string();

    if let async_openai::error::OpenAIError::ApiError(ref api_err) = e {
        // Try to extract status code from the error code or message
        let (category, code, status) = if api_err.code.as_deref().is_some_and(|c| c.contains("401"))
            || error_string.contains("401")
        {
            (ErrorCategory::Unauthorized, "model.openai_responses.unauthorized", Some(401u16))
        } else if api_err.code.as_deref().is_some_and(|c| c.contains("429"))
            || error_string.contains("429")
            || error_string.contains("rate")
        {
            (ErrorCategory::RateLimited, "model.openai_responses.rate_limited", Some(429u16))
        } else if error_string.contains("500")
            || error_string.contains("502")
            || error_string.contains("503")
            || error_string.contains("504")
            || error_string.contains("529")
        {
            (ErrorCategory::Unavailable, "model.openai_responses.unavailable", None)
        } else {
            (ErrorCategory::Internal, "model.openai_responses.api_error", None)
        };

        let mut err = AdkError::new(
            ErrorComponent::Model,
            category,
            code,
            format!("OpenAI Responses API error: {api_err}"),
        )
        .with_provider("openai-responses");
        if let Some(sc) = status {
            err = err.with_upstream_status(sc);
        }
        return err;
    }

    // Reqwest / network errors → Unavailable (retryable)
    if let async_openai::error::OpenAIError::Reqwest(_) = e {
        return AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.openai_responses.request",
            format!("OpenAI Responses API network error: {error_string}"),
        )
        .with_provider("openai-responses");
    }

    // Stream errors → Unavailable
    if let async_openai::error::OpenAIError::StreamError(_) = e {
        return AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.openai_responses.stream",
            format!("OpenAI Responses API stream error: {error_string}"),
        )
        .with_provider("openai-responses");
    }

    // JSON deserialization → Internal
    if let async_openai::error::OpenAIError::JSONDeserialize(_, _) = e {
        return AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.openai_responses.parse",
            format!("OpenAI Responses API parse error: {error_string}"),
        )
        .with_provider("openai-responses");
    }

    // Fallback
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Internal,
        "model.openai_responses.unknown",
        format!("OpenAI Responses API error: {error_string}"),
    )
    .with_provider("openai-responses")
}

#[async_trait]
impl Llm for OpenAIResponsesClient {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let usage_span = adk_telemetry::llm_generate_span("openai-responses", &self.model, stream);

        let create_request = responses_convert::build_create_response(
            &self.model,
            &request,
            self.reasoning_effort,
            self.reasoning_summary,
        )?;

        let uses_native_tools = responses_convert::request_uses_native_tools(&request);

        if stream && !uses_native_tools {
            // Explicitly set stream=true — async-openai's create_stream() does NOT
            // set this field automatically, causing the server to return JSON instead
            // of text/event-stream, which triggers an InvalidContentType error.
            let mut create_request = create_request;
            create_request.stream = Some(true);

            let event_stream = self
                .client
                .responses()
                .create_stream(create_request)
                .await
                .map_err(map_openai_error)?;

            let response_stream = event_stream.filter_map(|event_result| async {
                match event_result {
                    Ok(event) => {
                        use async_openai::types::responses::ResponseStreamEvent;
                        match event {
                            ResponseStreamEvent::ResponseOutputTextDelta(evt) => {
                                Some(Ok(LlmResponse {
                                    content: Some(Content {
                                        role: "model".to_string(),
                                        parts: vec![Part::Text { text: evt.delta }],
                                    }),
                                    partial: true,
                                    turn_complete: false,
                                    ..Default::default()
                                }))
                            }

                            ResponseStreamEvent::ResponseReasoningSummaryTextDelta(evt) => {
                                Some(Ok(LlmResponse {
                                    content: Some(Content {
                                        role: "model".to_string(),
                                        parts: vec![Part::Thinking {
                                            thinking: evt.delta,
                                            signature: None,
                                        }],
                                    }),
                                    partial: true,
                                    turn_complete: false,
                                    ..Default::default()
                                }))
                            }

                            // ResponseCompleted carries the authoritative response with
                            // correct function call names, usage, and finish reason.
                            // We extract only function calls (text was already streamed
                            // via delta events) and mark the turn complete.
                            ResponseStreamEvent::ResponseCompleted(evt) => {
                                let full = responses_convert::from_response(&evt.response);
                                // Extract only non-textual protocol parts (text/thinking were already
                                // streamed via delta events, but tool protocol items need to survive).
                                let trailing_parts: Vec<Part> = full
                                    .content
                                    .as_ref()
                                    .map(|c| {
                                        c.parts
                                            .iter()
                                            .filter(|part| {
                                                !matches!(
                                                    part,
                                                    Part::Text { .. } | Part::Thinking { .. }
                                                )
                                            })
                                            .cloned()
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                let content = if trailing_parts.is_empty() {
                                    None
                                } else {
                                    Some(Content {
                                        role: "model".to_string(),
                                        parts: trailing_parts,
                                    })
                                };

                                Some(Ok(LlmResponse {
                                    content,
                                    usage_metadata: full.usage_metadata,
                                    finish_reason: full.finish_reason,
                                    provider_metadata: full.provider_metadata,
                                    partial: false,
                                    turn_complete: true,
                                    ..Default::default()
                                }))
                            }

                            ResponseStreamEvent::ResponseFailed(evt) => {
                                let (error_code, error_message) =
                                    if let Some(err) = &evt.response.error {
                                        (Some(err.code.clone()), Some(err.message.clone()))
                                    } else {
                                        (
                                            Some("unknown".to_string()),
                                            Some("Response failed".to_string()),
                                        )
                                    };
                                Some(Ok(LlmResponse {
                                    error_code,
                                    error_message,
                                    turn_complete: true,
                                    ..Default::default()
                                }))
                            }

                            ResponseStreamEvent::ResponseError(evt) => Some(Ok(LlmResponse {
                                error_code: evt.code.or_else(|| Some("error".to_string())),
                                error_message: Some(evt.message),
                                turn_complete: true,
                                ..Default::default()
                            })),

                            // Skip all other events
                            _ => None,
                        }
                    }
                    Err(e) => Some(Err(map_openai_error(e))),
                }
            });

            Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
        } else {
            if stream && uses_native_tools {
                adk_telemetry::debug!(
                    "OpenAI native tools detected; using non-streaming responses path to avoid upstream SSE item parsing drift"
                );
            }

            // Non-streaming path
            let client = self.client.clone();
            let retry_config = self.retry_config.clone();

            let response_stream = try_stream! {
                let response = execute_with_retry(
                    &retry_config,
                    is_retryable_model_error,
                    || {
                        let client = client.clone();
                        let req = create_request.clone();
                        async move {
                            client
                                .responses()
                                .create(req)
                                .await
                                .map_err(map_openai_error)
                        }
                    },
                )
                .await?;

                let mut adk_response = responses_convert::from_response(&response);
                adk_response.turn_complete = true;
                adk_response.partial = false;
                yield adk_response;
            };

            Ok(crate::usage_tracking::with_usage_tracking(Box::pin(response_stream), usage_span))
        }
    }
}
