//! OpenAI Responses API client implementation.

use super::config::{OpenAIReasoningEffort, OpenAIResponsesConfig, ReasoningSummary};
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
    reasoning_effort: Option<OpenAIReasoningEffort>,
    reasoning_summary: Option<ReasoningSummary>,
    retry_config: RetryConfig,
    /// HTTP client for direct API calls (compaction, polling, etc.).
    http: reqwest::Client,
    /// API key for direct HTTP requests.
    api_key: String,
    /// Base URL for the API (defaults to `https://api.openai.com/v1`).
    base_url: String,
    /// Whether Open Responses mode is enabled for third-party compatibility.
    ///
    /// When `true`, relaxes strict OpenAI field validation and allows
    /// connecting to Open Responses-compatible endpoints without an OpenAI API key.
    open_responses_mode: bool,
}

impl OpenAIResponsesClient {
    /// Create a new Responses API client from the given config.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with `InvalidInput` if `api_key` is empty and
    /// Open Responses mode is not enabled with a custom base URL.
    pub fn new(config: OpenAIResponsesConfig) -> Result<Self, AdkError> {
        let reasoning_effort = config.reasoning_effort.map(OpenAIReasoningEffort::from);
        Self::new_inner(config, reasoning_effort)
    }

    /// Create a Responses API client with the complete reasoning-effort vocabulary.
    ///
    /// This constructor supports the `none`, `minimal`, `xhigh`, and `max` values
    /// used across current OpenAI model generations while retaining the original
    /// configuration field for backward compatibility.
    pub fn new_with_reasoning_effort(
        config: OpenAIResponsesConfig,
        reasoning_effort: OpenAIReasoningEffort,
    ) -> Result<Self, AdkError> {
        Self::new_inner(config, Some(reasoning_effort))
    }

    fn new_inner(
        config: OpenAIResponsesConfig,
        reasoning_effort: Option<OpenAIReasoningEffort>,
    ) -> Result<Self, AdkError> {
        let open_responses_mode = config.open_responses_mode.unwrap_or(false);

        // In Open Responses mode with a custom base URL, allow empty API keys
        // since third-party providers (LM Studio, Ollama, vLLM) may not require
        // OpenAI-specific authentication.
        let has_custom_base_url = config.base_url.is_some();
        if config.api_key.is_empty() && !(open_responses_mode && has_custom_base_url) {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openai_responses.invalid_config",
                "OpenAI Responses API key must not be empty",
            )
            .with_provider("openai-responses"));
        }

        // Use a placeholder API key for Open Responses mode when no key is provided.
        // This satisfies the async-openai client requirement for a non-empty key
        // while the actual endpoint may not validate it.
        let effective_api_key = if config.api_key.is_empty() && open_responses_mode {
            "open-responses-no-key".to_string()
        } else {
            config.api_key.clone()
        };

        let mut openai_config =
            async_openai::config::OpenAIConfig::new().with_api_key(&effective_api_key);
        if let Some(org_id) = &config.organization_id {
            openai_config = openai_config.with_org_id(org_id);
        }
        if let Some(base_url) = &config.base_url {
            openai_config = openai_config.with_api_base(base_url);
        }
        let client = async_openai::Client::with_config(openai_config);
        let client = if matches!(reasoning_effort, Some(OpenAIReasoningEffort::Max)) {
            with_max_reasoning_middleware(client)
        } else {
            client
        };

        let base_url =
            config.base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        Ok(Self {
            client,
            model: config.model,
            reasoning_effort,
            reasoning_summary: config.reasoning_summary,
            retry_config: RetryConfig::default(),
            http: reqwest::Client::new(),
            api_key: config.api_key,
            base_url,
            open_responses_mode,
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

    /// Get a reference to the underlying async-openai client.
    pub(crate) fn openai_client(
        &self,
    ) -> &async_openai::Client<async_openai::config::OpenAIConfig> {
        &self.client
    }

    /// Get the HTTP client for direct API calls.
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Get the API key.
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Get the base URL.
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether Open Responses mode is enabled.
    ///
    /// When `true`, the client relaxes strict OpenAI field validation for
    /// compatibility with third-party Open Responses-compatible endpoints.
    pub fn is_open_responses_mode(&self) -> bool {
        self.open_responses_mode
    }
}

fn with_max_reasoning_middleware(
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
) -> async_openai::Client<async_openai::config::OpenAIConfig> {
    use async_openai::error::OpenAIError;
    use async_openai::middleware::retry::OpenAIRetryLayer;
    use async_openai::middleware::{HttpRequestFactory, ReqwestService};
    use tower::ServiceExt;

    let transport = tower::ServiceBuilder::new()
        .layer(OpenAIRetryLayer::default())
        .service(ReqwestService::default());
    let service = tower::service_fn(move |factory: HttpRequestFactory| {
        let transport = transport.clone();
        async move {
            let original = factory.clone();
            let rewritten = HttpRequestFactory::new(move || {
                let original = original.clone();
                async move {
                    let mut request = original.build().await?;
                    if request.url().path().ends_with("/responses") {
                        let body =
                            request.body().and_then(|body| body.as_bytes()).ok_or_else(|| {
                                OpenAIError::InvalidArgument(
                                    "Responses request body was not replayable".to_string(),
                                )
                            })?;
                        let body_text = String::from_utf8_lossy(body).into_owned();
                        let mut value: serde_json::Value = serde_json::from_slice(body)
                            .map_err(|error| OpenAIError::JSONDeserialize(error, body_text))?;
                        set_max_reasoning_effort(&mut value)?;
                        let encoded = serde_json::to_vec(&value).map_err(|error| {
                            OpenAIError::InvalidArgument(format!(
                                "failed to serialize max-reasoning request: {error}"
                            ))
                        })?;
                        *request.body_mut() = Some(encoded.into());
                    }
                    Ok(request)
                }
            });
            let response = transport.oneshot(rewritten).await?;
            normalize_max_reasoning_response(response).await
        }
    });

    client.with_http_service(service)
}

async fn normalize_max_reasoning_response(
    response: reqwest_openai::Response,
) -> Result<reqwest_openai::Response, async_openai::error::OpenAIError> {
    use reqwest_openai::ResponseBuilderExt;

    if !response.status().is_success() {
        return Ok(response);
    }

    let is_event_stream = response
        .headers()
        .get(reqwest_openai::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let status = response.status();
    let version = response.version();
    let url = response.url().clone();
    let mut headers = response.headers().clone();
    headers.remove(reqwest_openai::header::CONTENT_LENGTH);

    let body = if is_event_stream {
        let mut upstream = response.bytes_stream();
        let normalized = async_stream::stream! {
            let mut pending = Vec::new();
            while let Some(chunk) = upstream.next().await {
                match chunk {
                    Ok(chunk) => {
                        pending.extend_from_slice(&chunk);
                        while let Some(line_end) = pending.iter().position(|byte| *byte == b'\n') {
                            let line: Vec<u8> = pending.drain(..=line_end).collect();
                            yield Ok::<Vec<u8>, reqwest_openai::Error>(normalize_max_reasoning_bytes(&line));
                        }
                    }
                    Err(error) => yield Err(error),
                }
            }
            if !pending.is_empty() {
                yield Ok::<Vec<u8>, reqwest_openai::Error>(normalize_max_reasoning_bytes(&pending));
            }
        };
        reqwest_openai::Body::wrap_stream(normalized)
    } else {
        let bytes = response.bytes().await.map_err(async_openai::error::OpenAIError::Reqwest)?;
        reqwest_openai::Body::from(normalize_max_reasoning_bytes(&bytes))
    };

    let mut rebuilt =
        http::Response::builder().status(status).version(version).url(url).body(body).map_err(
            |error| {
                async_openai::error::OpenAIError::InvalidArgument(format!(
                    "failed to rebuild max-reasoning response: {error}"
                ))
            },
        )?;
    *rebuilt.headers_mut() = headers;
    Ok(reqwest_openai::Response::from(rebuilt))
}

fn normalize_max_reasoning_bytes(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes)
        .replace("\"effort\":\"max\"", "\"effort\":\"xhigh\"")
        .replace("\"effort\": \"max\"", "\"effort\": \"xhigh\"")
        .into_bytes()
}

fn set_max_reasoning_effort(
    value: &mut serde_json::Value,
) -> Result<(), async_openai::error::OpenAIError> {
    let reasoning =
        value.get_mut("reasoning").and_then(serde_json::Value::as_object_mut).ok_or_else(|| {
            async_openai::error::OpenAIError::InvalidArgument(
                "max reasoning requires a Responses reasoning object".to_string(),
            )
        })?;
    reasoning.insert("effort".to_string(), serde_json::Value::String("max".to_string()));
    Ok(())
}

/// Map an `async_openai::error::OpenAIError` to an `AdkError`.
pub(super) fn map_openai_error(e: async_openai::error::OpenAIError) -> AdkError {
    let error_string = e.to_string();

    if let async_openai::error::OpenAIError::ApiError(ref api_err) = e {
        let status_code = api_err.status_code.as_u16();
        let (category, code) = match status_code {
            401 => (ErrorCategory::Unauthorized, "model.openai_responses.unauthorized"),
            403 => (ErrorCategory::Forbidden, "model.openai_responses.forbidden"),
            404 => (ErrorCategory::NotFound, "model.openai_responses.not_found"),
            408 => (ErrorCategory::Timeout, "model.openai_responses.timeout"),
            429 => (ErrorCategory::RateLimited, "model.openai_responses.rate_limited"),
            500 | 502 | 503 | 504 | 529 => {
                (ErrorCategory::Unavailable, "model.openai_responses.unavailable")
            }
            400..=499 => (ErrorCategory::InvalidInput, "model.openai_responses.invalid_request"),
            _ => (ErrorCategory::Internal, "model.openai_responses.api_error"),
        };

        return AdkError::new(
            ErrorComponent::Model,
            category,
            code,
            format!("OpenAI Responses API error: {api_err}"),
        )
        .with_provider("openai-responses")
        .with_upstream_status(status_code);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::config::OpenAIResponsesConfig;
    use async_openai::error::{ApiError, ApiErrorResponse, OpenAIError};

    fn api_error(status_code: u16) -> OpenAIError {
        OpenAIError::ApiError(ApiErrorResponse {
            status_code: reqwest::StatusCode::from_u16(status_code)
                .expect("status code should be valid"),
            api_error: ApiError {
                message: "upstream error".to_string(),
                r#type: None,
                param: None,
                code: None,
            },
        })
    }

    #[test]
    fn max_reasoning_rewrites_the_responses_wire_value() {
        let mut body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "reasoning": {"effort": "xhigh"}
        });

        set_max_reasoning_effort(&mut body).expect("max reasoning should be applied");

        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn max_reasoning_response_is_normalized_for_the_upstream_sdk() {
        let compact = br#"{"reasoning":{"effort":"max"}}"#;
        let spaced = br#"data: {"response":{"reasoning":{"effort": "max"}}}\n\n"#;

        assert_eq!(normalize_max_reasoning_bytes(compact), br#"{"reasoning":{"effort":"xhigh"}}"#);
        assert_eq!(
            normalize_max_reasoning_bytes(spaced),
            br#"data: {"response":{"reasoning":{"effort": "xhigh"}}}\n\n"#
        );
    }

    #[test]
    fn maps_api_error_statuses() {
        let cases = [
            (400, ErrorCategory::InvalidInput, "model.openai_responses.invalid_request", false),
            (401, ErrorCategory::Unauthorized, "model.openai_responses.unauthorized", false),
            (403, ErrorCategory::Forbidden, "model.openai_responses.forbidden", false),
            (404, ErrorCategory::NotFound, "model.openai_responses.not_found", false),
            (408, ErrorCategory::Timeout, "model.openai_responses.timeout", true),
            (418, ErrorCategory::InvalidInput, "model.openai_responses.invalid_request", false),
            (429, ErrorCategory::RateLimited, "model.openai_responses.rate_limited", true),
            (500, ErrorCategory::Unavailable, "model.openai_responses.unavailable", true),
            (503, ErrorCategory::Unavailable, "model.openai_responses.unavailable", true),
            (529, ErrorCategory::Unavailable, "model.openai_responses.unavailable", true),
            (599, ErrorCategory::Internal, "model.openai_responses.api_error", false),
        ];

        for (status, category, code, retryable) in cases {
            let error = map_openai_error(api_error(status));

            assert_eq!(
                (
                    error.category,
                    error.code,
                    error.is_retryable(),
                    error.details.upstream_status_code,
                ),
                (category, code, retryable, Some(status))
            );
        }
    }

    #[test]
    fn test_new_rejects_empty_api_key_without_open_responses_mode() {
        let config = OpenAIResponsesConfig::new("", "gpt-4o");
        let result = OpenAIResponsesClient::new(config);
        match result {
            Err(err) => assert_eq!(err.code, "model.openai_responses.invalid_config"),
            Ok(_) => panic!("expected error for empty API key"),
        }
    }

    #[test]
    fn test_new_rejects_empty_api_key_with_open_responses_mode_but_no_base_url() {
        // Open Responses mode alone is not enough — a custom base URL is also required
        let config = OpenAIResponsesConfig::new("", "local-model").with_open_responses_mode(true);
        let result = OpenAIResponsesClient::new(config);
        match result {
            Err(err) => assert_eq!(err.code, "model.openai_responses.invalid_config"),
            Ok(_) => panic!("expected error for empty API key without base URL"),
        }
    }

    #[test]
    fn test_new_allows_empty_api_key_with_open_responses_mode_and_base_url() {
        // Open Responses mode + custom base URL should allow empty API key
        let config = OpenAIResponsesConfig::new("", "local-model")
            .with_open_responses_mode(true)
            .with_base_url("http://localhost:1234/v1");
        let result = OpenAIResponsesClient::new(config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_open_responses_mode_stored_on_client() {
        let config = OpenAIResponsesConfig::new("", "local-model")
            .with_open_responses_mode(true)
            .with_base_url("http://localhost:1234/v1");
        let client = OpenAIResponsesClient::new(config).unwrap();
        assert!(client.is_open_responses_mode());
        assert_eq!(client.base_url(), "http://localhost:1234/v1");
    }

    #[test]
    fn test_new_with_api_key_and_open_responses_mode() {
        // Even with Open Responses mode, providing an API key should work fine
        let config = OpenAIResponsesConfig::new("sk-test", "local-model")
            .with_open_responses_mode(true)
            .with_base_url("http://localhost:1234/v1");
        let client = OpenAIResponsesClient::new(config).unwrap();
        assert!(client.is_open_responses_mode());
        assert_eq!(client.api_key(), "sk-test");
    }

    #[test]
    fn test_new_without_open_responses_mode_stores_false() {
        let config = OpenAIResponsesConfig::new("sk-test", "gpt-4o");
        let client = OpenAIResponsesClient::new(config).unwrap();
        assert!(!client.is_open_responses_mode());
    }

    #[test]
    fn test_custom_base_url_without_open_responses_mode_still_requires_api_key() {
        // Custom base URL alone doesn't bypass the API key requirement
        let config =
            OpenAIResponsesConfig::new("", "local-model").with_base_url("http://localhost:1234/v1");
        let result = OpenAIResponsesClient::new(config);
        assert!(result.is_err());
    }

    #[test]
    fn test_custom_base_url_with_api_key_works_without_open_responses_mode() {
        let config = OpenAIResponsesConfig::new("sk-test", "local-model")
            .with_base_url("http://localhost:1234/v1");
        let client = OpenAIResponsesClient::new(config).unwrap();
        assert!(!client.is_open_responses_mode());
        assert_eq!(client.base_url(), "http://localhost:1234/v1");
    }
}
