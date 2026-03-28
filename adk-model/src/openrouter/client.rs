//! Shared OpenRouter client scaffolding.

use super::chat::{OpenRouterChatRequest, OpenRouterChatResponse};
use super::config::OpenRouterConfig;
use super::discovery::{
    OpenRouterCredits, OpenRouterCreditsEnvelope, OpenRouterModel, OpenRouterModelEndpoints,
    OpenRouterModelEndpointsEnvelope, OpenRouterModelsEnvelope, OpenRouterProvider,
    OpenRouterProvidersEnvelope,
};
use super::error::{api_error_to_adk_error, stream_error_to_adk_error};
use super::responses::{OpenRouterResponse, OpenRouterResponsesRequest};
use super::stream::{
    OpenRouterChatStream, OpenRouterResponsesStream, OpenRouterSseDecoder, parse_chat_stream_frame,
    parse_responses_stream_frame,
};
use crate::retry::RetryConfig;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_stream::try_stream;
use futures::StreamExt;
use serde::de::DeserializeOwned;

/// Shared OpenRouter client used by the native APIs and the `Llm` adapter.
pub struct OpenRouterClient {
    http: reqwest::Client,
    config: OpenRouterConfig,
    retry_config: RetryConfig,
}

impl OpenRouterClient {
    /// Create a new OpenRouter client with shared default headers.
    pub fn new(config: OpenRouterConfig) -> Result<Self, AdkError> {
        let headers = config.default_headers()?;
        let http = reqwest::Client::builder().default_headers(headers).build().map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.client_build_failed",
                "Failed to build OpenRouter HTTP client",
            )
            .with_provider("openrouter")
            .with_source(err)
        })?;

        Ok(Self { http, config, retry_config: RetryConfig::default() })
    }

    /// Borrow the immutable client configuration.
    pub fn config(&self) -> &OpenRouterConfig {
        &self.config
    }

    /// Borrow the shared HTTP client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Borrow the configured default model name.
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Return a new client value with the provided retry configuration.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Replace the retry configuration in place.
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.retry_config = retry_config;
    }

    /// Borrow the active retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    /// Build an absolute endpoint URL for a relative OpenRouter API path.
    pub fn endpoint_url(&self, path: &str) -> String {
        self.config.endpoint_url(path)
    }

    /// Send a non-streaming chat-completions request to OpenRouter.
    pub async fn send_chat(
        &self,
        mut request: OpenRouterChatRequest,
    ) -> Result<OpenRouterChatResponse, AdkError> {
        if request.model.trim().is_empty() {
            request.model = self.config.model.clone();
        }

        let response = self
            .http_client()
            .post(self.endpoint_url("/chat/completions"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openrouter.request_failed",
                    "OpenRouter request failed before the server returned a response",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.response_read_failed",
                "OpenRouter returned a response body that could not be read",
            )
            .with_provider("openrouter")
            .with_source(err)
        })?;

        if !status.is_success() {
            return Err(api_error_to_adk_error(status.as_u16(), &headers, &body));
        }

        serde_json::from_str::<OpenRouterChatResponse>(&body).map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.invalid_response",
                "OpenRouter returned a chat response that could not be parsed",
            )
            .with_provider("openrouter")
            .with_upstream_status(status.as_u16())
            .with_source(err)
        })
    }

    /// Send a streaming chat-completions request to OpenRouter.
    pub async fn send_chat_stream(
        &self,
        mut request: OpenRouterChatRequest,
    ) -> Result<OpenRouterChatStream, AdkError> {
        if request.model.trim().is_empty() {
            request.model = self.config.model.clone();
        }
        request.stream = Some(true);

        let response = self
            .http_client()
            .post(self.endpoint_url("/chat/completions"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openrouter.request_failed",
                    "OpenRouter request failed before the server returned a response",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = response.text().await.map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Internal,
                    "model.openrouter.response_read_failed",
                    "OpenRouter returned a response body that could not be read",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;
            return Err(api_error_to_adk_error(status.as_u16(), &headers, &body));
        }

        let stream = try_stream! {
            let mut decoder = OpenRouterSseDecoder::new();
            let mut byte_stream = response.bytes_stream();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk.map_err(|err| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::Unavailable,
                        "model.openrouter.chat_stream_read_failed",
                        "OpenRouter chat stream could not be read",
                    )
                    .with_provider("openrouter")
                    .with_source(err)
                })?;

                for frame in decoder.push(&String::from_utf8_lossy(&chunk)) {
                    if let Some(item) = parse_chat_stream_frame(&frame)? {
                        match item {
                            super::stream::OpenRouterChatStreamItem::Error(error) => {
                                Err(stream_error_to_adk_error(&error, frame.retry))?
                            }
                            other => yield other,
                        }
                    }
                }
            }

            for frame in decoder.finish() {
                if let Some(item) = parse_chat_stream_frame(&frame)? {
                    match item {
                        super::stream::OpenRouterChatStreamItem::Error(error) => {
                            Err(stream_error_to_adk_error(&error, frame.retry))?
                        }
                        other => yield other,
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Send a non-streaming Responses API request to OpenRouter.
    pub async fn create_response(
        &self,
        mut request: OpenRouterResponsesRequest,
    ) -> Result<OpenRouterResponse, AdkError> {
        if request.model.as_deref().map(str::trim).is_none_or(str::is_empty) {
            request.model = Some(self.config.model.clone());
        }

        let response = self
            .http_client()
            .post(self.endpoint_url("/responses"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openrouter.request_failed",
                    "OpenRouter request failed before the server returned a response",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.response_read_failed",
                "OpenRouter returned a response body that could not be read",
            )
            .with_provider("openrouter")
            .with_source(err)
        })?;

        if !status.is_success() {
            return Err(api_error_to_adk_error(status.as_u16(), &headers, &body));
        }

        serde_json::from_str::<OpenRouterResponse>(&body).map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.invalid_response",
                "OpenRouter returned a Responses API payload that could not be parsed",
            )
            .with_provider("openrouter")
            .with_upstream_status(status.as_u16())
            .with_source(err)
        })
    }

    /// Send a streaming Responses API request to OpenRouter.
    pub async fn create_response_stream(
        &self,
        mut request: OpenRouterResponsesRequest,
    ) -> Result<OpenRouterResponsesStream, AdkError> {
        if request.model.as_deref().map(str::trim).is_none_or(str::is_empty) {
            request.model = Some(self.config.model.clone());
        }
        request.stream = Some(true);

        let response = self
            .http_client()
            .post(self.endpoint_url("/responses"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openrouter.request_failed",
                    "OpenRouter request failed before the server returned a response",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = response.text().await.map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Internal,
                    "model.openrouter.response_read_failed",
                    "OpenRouter returned a response body that could not be read",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;
            return Err(api_error_to_adk_error(status.as_u16(), &headers, &body));
        }

        let stream = try_stream! {
            let mut decoder = OpenRouterSseDecoder::new();
            let mut byte_stream = response.bytes_stream();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk.map_err(|err| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::Unavailable,
                        "model.openrouter.responses_stream_read_failed",
                        "OpenRouter responses stream could not be read",
                    )
                    .with_provider("openrouter")
                    .with_source(err)
                })?;

                for frame in decoder.push(&String::from_utf8_lossy(&chunk)) {
                    if let Some(item) = parse_responses_stream_frame(&frame)? {
                        match item {
                            super::stream::OpenRouterResponsesStreamItem::Error(error) => {
                                Err(stream_error_to_adk_error(&error, frame.retry))?
                            }
                            other => yield other,
                        }
                    }
                }
            }

            for frame in decoder.finish() {
                if let Some(item) = parse_responses_stream_frame(&frame)? {
                    match item {
                        super::stream::OpenRouterResponsesStreamItem::Error(error) => {
                            Err(stream_error_to_adk_error(&error, frame.retry))?
                        }
                        other => yield other,
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// List models available from OpenRouter discovery.
    pub async fn list_models(&self) -> Result<Vec<OpenRouterModel>, AdkError> {
        Ok(self
            .get_discovery_json::<OpenRouterModelsEnvelope>(
                "/models",
                "model.openrouter.invalid_models_response",
                "OpenRouter returned a models response that could not be parsed",
            )
            .await?
            .data)
    }

    /// List provider endpoints for a discovered OpenRouter model.
    pub async fn get_model_endpoints(
        &self,
        author: &str,
        slug: &str,
    ) -> Result<OpenRouterModelEndpoints, AdkError> {
        Ok(self
            .get_discovery_json::<OpenRouterModelEndpointsEnvelope>(
                &format!("/models/{author}/{slug}/endpoints"),
                "model.openrouter.invalid_model_endpoints_response",
                "OpenRouter returned a model endpoints response that could not be parsed",
            )
            .await?
            .data)
    }

    /// List providers available on OpenRouter.
    pub async fn list_providers(&self) -> Result<Vec<OpenRouterProvider>, AdkError> {
        Ok(self
            .get_discovery_json::<OpenRouterProvidersEnvelope>(
                "/providers",
                "model.openrouter.invalid_providers_response",
                "OpenRouter returned a providers response that could not be parsed",
            )
            .await?
            .data)
    }

    /// Get the authenticated account's remaining credits.
    pub async fn get_credits(&self) -> Result<OpenRouterCredits, AdkError> {
        Ok(self
            .get_discovery_json::<OpenRouterCreditsEnvelope>(
                "/credits",
                "model.openrouter.invalid_credits_response",
                "OpenRouter returned a credits response that could not be parsed",
            )
            .await?
            .data)
    }

    async fn get_discovery_json<T>(
        &self,
        path: &str,
        invalid_code: &'static str,
        invalid_message: &'static str,
    ) -> Result<T, AdkError>
    where
        T: DeserializeOwned,
    {
        let response =
            self.http_client().get(self.endpoint_url(path)).send().await.map_err(|err| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openrouter.request_failed",
                    "OpenRouter request failed before the server returned a response",
                )
                .with_provider("openrouter")
                .with_source(err)
            })?;

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openrouter.response_read_failed",
                "OpenRouter returned a response body that could not be read",
            )
            .with_provider("openrouter")
            .with_source(err)
        })?;

        if !status.is_success() {
            return Err(api_error_to_adk_error(status.as_u16(), &headers, &body));
        }

        serde_json::from_str::<T>(&body).map_err(|err| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                invalid_code,
                invalid_message,
            )
            .with_provider("openrouter")
            .with_upstream_status(status.as_u16())
            .with_source(err)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::OpenRouterClient;
    use crate::openrouter::chat::{
        OpenRouterChatChoice, OpenRouterChatMessage, OpenRouterChatMessageContent,
        OpenRouterChatRequest, OpenRouterChatResponse, OpenRouterChatUsage,
        OpenRouterFunctionDescription, OpenRouterPlugin, OpenRouterProviderMaxPrice,
        OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterResponseFormat,
        OpenRouterTool, OpenRouterToolChoice,
    };
    use crate::openrouter::config::OpenRouterConfig;
    use crate::openrouter::discovery::{OpenRouterBigNumber, OpenRouterEndpointStatus};
    use crate::openrouter::responses::{
        OpenRouterResponse, OpenRouterResponseInput, OpenRouterResponseOutputItem,
        OpenRouterResponseTool, OpenRouterResponsesRequest, OpenRouterResponsesUsage,
    };
    use crate::openrouter::stream::{OpenRouterChatStreamItem, OpenRouterResponsesStreamItem};
    use adk_core::ErrorCategory;
    use futures::StreamExt;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn list_models_error_for_status(
        status: u16,
        body: serde_json::Value,
        retry_after: Option<&str>,
    ) -> adk_core::AdkError {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(status).set_body_json(body);
        if let Some(retry_after) = retry_after {
            response = response.insert_header("retry-after", retry_after);
        }

        Mock::given(method("GET")).and(path("/models")).respond_with(response).mount(&server).await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        client.list_models().await.expect_err("request should fail with an OpenRouter error")
    }

    #[tokio::test]
    async fn send_chat_uses_default_model_when_request_model_is_empty() {
        let server = MockServer::start().await;
        let request_body = json!({
            "model": "openai/gpt-5.2",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_json(&request_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "model": "openai/gpt-5.2",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "world"
                        },
                        "finish_reason": "stop"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2")
                .with_base_url(format!("{}/", server.uri())),
        )
        .expect("client should build");

        let response = client
            .send_chat(OpenRouterChatRequest {
                model: String::new(),
                messages: vec![OpenRouterChatMessage {
                    role: "user".to_string(),
                    content: Some(OpenRouterChatMessageContent::Text("hello".to_string())),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .await
            .expect("chat request should succeed");

        assert_eq!(response.id.as_deref(), Some("chatcmpl-1"));
        assert_eq!(response.model.as_deref(), Some("openai/gpt-5.2"));
    }

    #[tokio::test]
    async fn send_chat_stream_yields_chunk_and_done() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"openai/gpt-5.2\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hel\"}}]}\n\n\
                 data: [DONE]\n\n",
            ))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut stream = client
            .send_chat_stream(OpenRouterChatRequest {
                model: String::new(),
                messages: vec![OpenRouterChatMessage {
                    role: "user".to_string(),
                    content: Some(OpenRouterChatMessageContent::Text("hello".to_string())),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .await
            .expect("stream should start");

        let first = stream.next().await.expect("first item").expect("first item should parse");
        let second = stream.next().await.expect("second item").expect("second item should parse");

        match first {
            OpenRouterChatStreamItem::Chunk(chunk) => {
                assert_eq!(
                    chunk
                        .choices
                        .first()
                        .and_then(|choice| choice.delta.as_ref())
                        .and_then(|delta| delta.content.as_ref()),
                    Some(&OpenRouterChatMessageContent::Text("hel".to_string()))
                );
            }
            other => panic!("expected chunk item, got {other:?}"),
        }
        assert!(matches!(second, OpenRouterChatStreamItem::Done));
    }

    #[tokio::test]
    async fn send_chat_stream_surfaces_error_frame_as_adk_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    "retry: 1500\n\
                     data: {\"error\":{\"message\":\"Rate limit exceeded\",\"code\":429,\"provider_name\":\"openrouter\"}}\n\n",
                ),
            )
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut stream = client
            .send_chat_stream(OpenRouterChatRequest {
                model: String::new(),
                messages: vec![OpenRouterChatMessage {
                    role: "user".to_string(),
                    content: Some(OpenRouterChatMessageContent::Text("hello".to_string())),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .await
            .expect("stream should start");

        let first = stream.next().await.expect("stream should yield an error");
        let error = first.expect_err("stream item should be normalized into AdkError");

        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.code, "model.openrouter.rate_limited");
        assert_eq!(error.details.provider.as_deref(), Some("openrouter"));
        assert_eq!(error.retry.retry_after_ms, Some(1500));
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn chat_request_serializes_openrouter_specific_fields() {
        let request = OpenRouterChatRequest {
            model: "openai/gpt-5.2".to_string(),
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text("hello".to_string())),
                ..Default::default()
            }],
            tools: Some(vec![OpenRouterTool {
                kind: "function".to_string(),
                function: Some(OpenRouterFunctionDescription {
                    name: "lookup_weather".to_string(),
                    description: Some("Fetch the weather".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            tool_choice: Some(OpenRouterToolChoice::Mode("auto".to_string())),
            plugins: Some(vec![OpenRouterPlugin {
                id: "web".to_string(),
                enabled: Some(true),
                ..Default::default()
            }]),
            provider: Some(OpenRouterProviderPreferences {
                order: Some(vec!["openai".to_string(), "anthropic".to_string()]),
                max_price: Some(OpenRouterProviderMaxPrice {
                    prompt: Some(json!(0.5)),
                    completion: Some(json!(1.0)),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("high".to_string()),
                summary: Some("auto".to_string()),
                ..Default::default()
            }),
            response_format: Some(OpenRouterResponseFormat {
                kind: "json_schema".to_string(),
                json_schema: Some(json!({
                    "name": "weather_report",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "temperature": { "type": "number" }
                        },
                        "required": ["temperature"]
                    }
                })),
                ..Default::default()
            }),
            models: Some(vec!["openai/gpt-5.2".to_string(), "openai/gpt-5-mini".to_string()]),
            route: Some("fallback".to_string()),
            ..Default::default()
        };

        let json = serde_json::to_value(&request).expect("request should serialize");

        assert_eq!(json["tools"][0]["function"]["name"], "lookup_weather");
        assert_eq!(json["plugins"][0]["id"], "web");
        assert_eq!(json["provider"]["order"][0], "openai");
        assert_eq!(json["provider"]["max_price"]["completion"], 1.0);
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["response_format"]["type"], "json_schema");
        assert_eq!(json["models"][1], "openai/gpt-5-mini");
        assert_eq!(json["route"], "fallback");
    }

    #[test]
    fn provider_routing_preferences_round_trip_losslessly() {
        let preferences = OpenRouterProviderPreferences {
            allow_fallbacks: Some(true),
            require_parameters: Some(true),
            data_collection: Some("deny".to_string()),
            zdr: Some(true),
            enforce_distillable_text: Some(true),
            order: Some(vec!["openai".to_string(), "anthropic".to_string()]),
            only: Some(vec!["openai".to_string()]),
            ignore: Some(vec!["some-provider".to_string()]),
            quantizations: Some(vec!["fp8".to_string()]),
            sort: Some("throughput".to_string()),
            max_price: Some(OpenRouterProviderMaxPrice {
                prompt: Some(json!(0.5)),
                completion: Some(json!(1.0)),
                image: Some(json!(0.25)),
                request: Some(json!(0.01)),
                ..Default::default()
            }),
            preferred_min_throughput: Some(25.0),
            preferred_max_latency: Some(750.0),
            ..Default::default()
        };

        let json = serde_json::to_value(&preferences).expect("preferences should serialize");
        let round_trip: OpenRouterProviderPreferences =
            serde_json::from_value(json.clone()).expect("preferences should deserialize");

        assert_eq!(round_trip, preferences);
        assert_eq!(json["order"][1], "anthropic");
        assert_eq!(json["max_price"]["request"], 0.01);
        assert_eq!(json["preferred_max_latency"], 750.0);
    }

    #[test]
    fn tool_calling_request_does_not_inject_provider_override_when_unspecified() {
        let request = OpenRouterChatRequest {
            model: "openai/gpt-5.2".to_string(),
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text("hello".to_string())),
                ..Default::default()
            }],
            tools: Some(vec![OpenRouterTool {
                kind: "function".to_string(),
                function: Some(OpenRouterFunctionDescription {
                    name: "lookup_weather".to_string(),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        }
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let json = serde_json::to_value(&request).expect("request should serialize");

        assert!(json.get("provider").is_none());
    }

    #[test]
    fn chat_response_round_trips_usage_and_choices() {
        let response = OpenRouterChatResponse {
            id: Some("chatcmpl-1".to_string()),
            model: Some("openai/gpt-5.2".to_string()),
            usage: Some(OpenRouterChatUsage {
                prompt_tokens: Some(11),
                completion_tokens: Some(7),
                total_tokens: Some(18),
                cost: Some(0.0012),
                is_byok: Some(false),
                ..Default::default()
            }),
            choices: vec![OpenRouterChatChoice {
                index: Some(0),
                message: Some(OpenRouterChatMessage {
                    role: "assistant".to_string(),
                    content: Some(OpenRouterChatMessageContent::Text("world".to_string())),
                    ..Default::default()
                }),
                finish_reason: Some("stop".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let json = serde_json::to_string(&response).expect("response should serialize");
        let round_trip: OpenRouterChatResponse =
            serde_json::from_str(&json).expect("response should deserialize");

        assert_eq!(round_trip.usage.as_ref().and_then(|usage| usage.cost), Some(0.0012));
        assert_eq!(
            round_trip
                .choices
                .first()
                .and_then(|choice| choice.message.as_ref())
                .map(|message| message.role.as_str()),
            Some("assistant")
        );
    }

    #[tokio::test]
    async fn create_response_uses_default_model_when_request_model_is_missing() {
        let server = MockServer::start().await;
        let request_body = json!({
            "model": "openai/gpt-5.2",
            "input": "hello"
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_json(&request_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "model": "openai/gpt-5.2",
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "output_text",
                                "text": "world"
                            }
                        ]
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2")
                .with_base_url(format!("{}/", server.uri())),
        )
        .expect("client should build");

        let response = client
            .create_response(OpenRouterResponsesRequest {
                input: Some(OpenRouterResponseInput::Text("hello".to_string())),
                ..Default::default()
            })
            .await
            .expect("responses request should succeed");

        assert_eq!(response.id.as_deref(), Some("resp_1"));
        assert_eq!(response.model.as_deref(), Some("openai/gpt-5.2"));
    }

    #[tokio::test]
    async fn create_response_stream_yields_event_and_done() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"item_id\":\"item-1\",\"content_index\":0,\"delta\":\"world\",\"logprobs\":[],\"sequence_number\":1}\n\n\
                 data: [DONE]\n\n",
            ))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let mut stream = client
            .create_response_stream(OpenRouterResponsesRequest {
                input: Some(OpenRouterResponseInput::Text("hello".to_string())),
                ..Default::default()
            })
            .await
            .expect("stream should start");

        let first = stream.next().await.expect("first item").expect("first item should parse");
        let second = stream.next().await.expect("second item").expect("second item should parse");

        match first {
            OpenRouterResponsesStreamItem::Event(event) => {
                assert_eq!(event.kind, "response.output_text.delta");
                assert_eq!(event.delta.as_deref(), Some("world"));
            }
            other => panic!("expected responses event, got {other:?}"),
        }
        assert!(matches!(second, OpenRouterResponsesStreamItem::Done));
    }

    #[test]
    fn responses_request_serializes_server_tools_reasoning_and_chaining() {
        let request = OpenRouterResponsesRequest {
            input: Some(OpenRouterResponseInput::Text("search for rust".to_string())),
            tools: Some(vec![
                OpenRouterResponseTool {
                    kind: "web_search_preview_2025_03_11".to_string(),
                    ..Default::default()
                },
                OpenRouterResponseTool {
                    kind: "function".to_string(),
                    name: Some("store_result".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "result": { "type": "string" }
                        },
                        "required": ["result"]
                    })),
                    ..Default::default()
                },
            ]),
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("medium".to_string()),
                summary: Some("auto".to_string()),
                ..Default::default()
            }),
            previous_response_id: Some("resp_previous".to_string()),
            models: Some(vec!["openai/gpt-5.2".to_string(), "openai/gpt-5-mini".to_string()]),
            route: Some("fallback".to_string()),
            ..Default::default()
        };

        let json = serde_json::to_value(&request).expect("request should serialize");

        assert_eq!(json["tools"][0]["type"], "web_search_preview_2025_03_11");
        assert_eq!(json["tools"][1]["name"], "store_result");
        assert_eq!(json["reasoning"]["summary"], "auto");
        assert_eq!(json["previous_response_id"], "resp_previous");
        assert_eq!(json["models"][1], "openai/gpt-5-mini");
        assert_eq!(json["route"], "fallback");
    }

    #[test]
    fn responses_response_round_trips_output_items_and_usage() {
        let response = OpenRouterResponse {
            id: Some("resp_1".to_string()),
            model: Some("openai/gpt-5.2".to_string()),
            status: Some("completed".to_string()),
            usage: Some(OpenRouterResponsesUsage {
                input_tokens: Some(15),
                output_tokens: Some(12),
                total_tokens: Some(27),
                cost: Some(0.0015),
                ..Default::default()
            }),
            output: vec![
                OpenRouterResponseOutputItem {
                    kind: "reasoning".to_string(),
                    summary: Some(vec![json!({
                        "type": "summary_text",
                        "text": "searched the web and synthesized the result"
                    })]),
                    ..Default::default()
                },
                OpenRouterResponseOutputItem {
                    kind: "web_search_call".to_string(),
                    status: Some("completed".to_string()),
                    ..Default::default()
                },
                OpenRouterResponseOutputItem {
                    kind: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: Some(json!([
                        {
                            "type": "output_text",
                            "text": "Rust is a systems language."
                        }
                    ])),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let json = serde_json::to_string(&response).expect("response should serialize");
        let round_trip: OpenRouterResponse =
            serde_json::from_str(&json).expect("response should deserialize");

        assert_eq!(round_trip.output.len(), 3);
        assert_eq!(round_trip.output[0].kind, "reasoning");
        assert_eq!(round_trip.output[1].kind, "web_search_call");
        assert_eq!(round_trip.usage.as_ref().and_then(|usage| usage.cost), Some(0.0015));
    }

    #[tokio::test]
    async fn list_models_parses_pricing_architecture_and_supported_parameters() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {
                        "id": "openai/gpt-4.1",
                        "canonical_slug": "openai/gpt-4.1",
                        "hugging_face_id": "openai/gpt-4.1",
                        "name": "GPT-4.1",
                        "created": 1710000000,
                        "description": "General-purpose model",
                        "pricing": {
                            "prompt": "0.000002",
                            "completion": 0.000008,
                            "request": "0",
                            "image_token": "0.000001",
                            "web_search": "0.01",
                            "discount": 0.15
                        },
                        "context_length": 128000,
                        "architecture": {
                            "tokenizer": "GPT",
                            "instruct_type": "chatml",
                            "modality": "text->text",
                            "input_modalities": ["text", "image"],
                            "output_modalities": ["text"]
                        },
                        "top_provider": {
                            "context_length": 128000,
                            "max_completion_tokens": 16384,
                            "is_moderated": true
                        },
                        "per_request_limits": {
                            "prompt_tokens": 8000,
                            "completion_tokens": 4000
                        },
                        "supported_parameters": ["temperature", "web_search_options"],
                        "default_parameters": {
                            "temperature": 0.7,
                            "top_p": 0.9,
                            "frequency_penalty": 0.1
                        },
                        "expiration_date": "2026-12-01"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let models = client.list_models().await.expect("models request should succeed");
        let model = models.first().expect("model should exist");

        assert_eq!(model.id, "openai/gpt-4.1");
        assert_eq!(model.supported_parameters, vec!["temperature", "web_search_options"]);
        assert_eq!(
            model.architecture.as_ref().map(|architecture| architecture.input_modalities.clone()),
            Some(vec!["text".to_string(), "image".to_string()])
        );
        assert_eq!(
            model.pricing.as_ref().and_then(|pricing| pricing.image_token.clone()),
            Some(OpenRouterBigNumber::String("0.000001".to_string()))
        );
        assert_eq!(
            model.top_provider.as_ref().and_then(|provider| provider.max_completion_tokens),
            Some(16384)
        );
        assert_eq!(
            model.default_parameters.as_ref().and_then(|defaults| defaults.temperature),
            Some(0.7)
        );
        assert_eq!(model.expiration_date.as_deref(), Some("2026-12-01"));
    }

    #[tokio::test]
    async fn get_model_endpoints_parses_latency_throughput_and_implicit_caching_metadata() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/models/openai/gpt-4.1/endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "id": "openai/gpt-4.1",
                    "name": "GPT-4.1",
                    "created": 1710000000,
                    "description": "General-purpose model",
                    "architecture": {
                        "tokenizer": "GPT",
                        "instruct_type": "chatml",
                        "modality": "text->text",
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    },
                    "endpoints": [
                        {
                            "name": "OpenAI: GPT-4.1",
                            "model_id": "openai/gpt-4.1",
                            "model_name": "GPT-4.1",
                            "context_length": 128000,
                            "pricing": {
                                "prompt": "0.000002",
                                "completion": "0.000008"
                            },
                            "provider_name": "OpenAI",
                            "tag": "openai",
                            "quantization": "fp16",
                            "max_completion_tokens": 16384,
                            "max_prompt_tokens": 128000,
                            "supported_parameters": ["temperature", "top_p"],
                            "status": 0,
                            "uptime_last_30m": 99.9,
                            "supports_implicit_caching": true,
                            "latency_last_30m": {
                                "p50": 120.0,
                                "p75": 180.0,
                                "p90": 240.0,
                                "p99": 900.0
                            },
                            "throughput_last_30m": {
                                "p50": 80.0,
                                "p75": 65.0,
                                "p90": 42.0,
                                "p99": 10.0
                            }
                        }
                    ]
                }
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let endpoints = client
            .get_model_endpoints("openai", "gpt-4.1")
            .await
            .expect("endpoints request should succeed");
        let endpoint = endpoints.endpoints.first().expect("endpoint should exist");

        assert_eq!(endpoints.id, "openai/gpt-4.1");
        assert_eq!(endpoint.provider_name.as_deref(), Some("OpenAI"));
        assert_eq!(endpoint.supported_parameters, vec!["temperature", "top_p"]);
        assert_eq!(endpoint.status, Some(OpenRouterEndpointStatus::Code(0)));
        assert_eq!(endpoint.supports_implicit_caching, Some(true));
        assert_eq!(endpoint.latency_last_30m.as_ref().and_then(|latency| latency.p99), Some(900.0));
        assert_eq!(
            endpoint.throughput_last_30m.as_ref().and_then(|throughput| throughput.p50),
            Some(80.0)
        );
    }

    #[tokio::test]
    async fn list_providers_returns_provider_entries() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/providers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    {
                        "name": "OpenAI",
                        "slug": "openai",
                        "privacy_policy_url": "https://openai.com/privacy",
                        "terms_of_service_url": "https://openai.com/terms",
                        "status_page_url": "https://status.openai.com"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let providers = client.list_providers().await.expect("providers request should succeed");

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].slug, "openai");
        assert_eq!(providers[0].status_page_url.as_deref(), Some("https://status.openai.com"));
    }

    #[tokio::test]
    async fn get_credits_returns_credit_totals() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "total_credits": 100.5,
                    "total_usage": 25.75
                }
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let credits = client.get_credits().await.expect("credits request should succeed");

        assert_eq!(credits.total_credits, 100.5);
        assert_eq!(credits.total_usage, 25.75);
    }

    #[tokio::test]
    async fn get_credits_surfaces_structured_forbidden_error_for_non_management_keys() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": {
                    "message": "Only management keys can fetch credits",
                    "provider_name": "openrouter"
                }
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::new(
            OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2").with_base_url(server.uri()),
        )
        .expect("client should build");

        let error = client
            .get_credits()
            .await
            .expect_err("credits request should fail for non-management keys");

        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert_eq!(error.details.provider.as_deref(), Some("openrouter"));
        assert_eq!(error.details.upstream_status_code, Some(403));
        assert_eq!(error.message, "Only management keys can fetch credits");
    }

    #[tokio::test]
    async fn status_402_maps_to_forbidden_insufficient_credits_error() {
        let error = list_models_error_for_status(
            402,
            json!({
                "error": {
                    "message": "Insufficient credits. Add more using https://openrouter.ai/credits",
                    "code": 402,
                    "provider_name": "openrouter"
                }
            }),
            None,
        )
        .await;

        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert_eq!(error.code, "model.openrouter.insufficient_credits");
        assert!(!error.retry.should_retry);
    }

    #[tokio::test]
    async fn status_429_maps_to_rate_limited_and_honors_retry_after() {
        let error = list_models_error_for_status(
            429,
            json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "code": 429,
                    "provider_name": "openrouter"
                }
            }),
            Some("9"),
        )
        .await;

        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.code, "model.openrouter.rate_limited");
        assert_eq!(error.retry.retry_after_ms, Some(9_000));
    }

    #[tokio::test]
    async fn status_503_maps_to_unavailable() {
        let error = list_models_error_for_status(
            503,
            json!({
                "error": {
                    "message": "Service temporarily unavailable",
                    "code": 503,
                    "provider_name": "openrouter"
                }
            }),
            None,
        )
        .await;

        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.code, "model.openrouter.unavailable");
        assert!(error.retry.should_retry);
    }

    #[tokio::test]
    async fn status_524_maps_to_timeout() {
        let error = list_models_error_for_status(
            524,
            json!({
                "error": {
                    "message": "Request timed out. Please try again later.",
                    "code": 524,
                    "provider_name": "openrouter"
                }
            }),
            None,
        )
        .await;

        assert_eq!(error.category, ErrorCategory::Timeout);
        assert_eq!(error.code, "model.openrouter.edge_timeout");
        assert!(error.retry.should_retry);
    }

    #[tokio::test]
    async fn status_529_maps_to_unavailable_provider_overload() {
        let error = list_models_error_for_status(
            529,
            json!({
                "error": {
                    "message": "Provider returned error",
                    "code": 529,
                    "provider_name": "openrouter"
                }
            }),
            None,
        )
        .await;

        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.code, "model.openrouter.provider_overloaded");
        assert!(error.retry.should_retry);
    }
}
