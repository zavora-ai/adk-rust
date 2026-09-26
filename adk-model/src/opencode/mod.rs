//! OpenCode Go and Zen integration using the existing protocol clients.
//!
//! Enable the `opencode` feature. API selection follows the published
//! endpoint table for each service; unknown models
//! require an explicit API selection. No network request is made during construction.

mod config;
pub use config::{OpenCodeApi, OpenCodeConfig};
mod service;
pub use service::OpenCodeService;

use crate::anthropic::{AnthropicClient, AnthropicConfig};
use crate::gemini::GeminiModel;
use crate::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponseStream, SchemaAdapter,
};
use async_trait::async_trait;
use http::header::{HeaderMap, HeaderValue, USER_AGENT};
use std::sync::Arc;

/// An OpenCode model bound to one conversation.
///
/// The selected protocol client owns wire conversion, streaming, usage, errors,
/// and tool calls. The OpenCode adapter supplies routing and conversation headers.
///
/// # Example
///
/// ```no_run
/// use adk_model::opencode::{OpenCodeClient, OpenCodeConfig, OpenCodeService};
/// let model = OpenCodeClient::new(
///     OpenCodeConfig::new(OpenCodeService::Go, std::env::var("OPENCODE_API_KEY")?, "deepseek-v4.1-flash")
///         .with_user_agent("example-coding-agent/1.0")
///         .with_session_id("conversation-42"),
/// )?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct OpenCodeClient {
    inner: Arc<dyn Llm>,
    api: OpenCodeApi,
}

impl OpenCodeClient {
    /// Builds an OpenCode client using the configured model's API.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for missing client/session identity, invalid
    /// HTTP headers or base URL, unknown models without an explicit API, or
    /// reasoning settings for a different API.
    pub fn new(config: OpenCodeConfig) -> Result<Self, AdkError> {
        let api = config.api.or_else(|| config.service.api(&config.model)).ok_or_else(|| {
            invalid("unknown OpenCode model; select its documented API with with_api")
        })?;
        let endpoint = reqwest::Url::parse(&config.base_url)
            .map_err(|_| invalid("invalid OpenCode base URL"))?;
        let local = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && local)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.path().trim_end_matches('/').ends_with("/v1")
        {
            return Err(invalid(
                "OpenCode base URL must use HTTPS (or loopback HTTP), end in /v1, and contain no credentials, query, or fragment",
            ));
        }
        let mut headers = HeaderMap::new();
        for (name, value) in [
            (USER_AGENT, &config.user_agent),
            (http::header::HeaderName::from_static("x-opencode-session"), &config.session_id),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(
                    "OpenCode requires a nonempty user agent and stable session ID",
                ));
            }
            headers.insert(
                name,
                HeaderValue::from_str(value).map_err(|_| {
                    invalid("OpenCode client/session identity must be a valid HTTP header value")
                })?,
            );
        }
        let base_url = config.base_url.trim_end_matches('/');
        let inner: Arc<dyn Llm> = match api {
            OpenCodeApi::ChatCompletions | OpenCodeApi::Responses => {
                if config.thinking.is_some()
                    || config.anthropic_effort.is_some()
                    || config.gemini_thinking.is_some()
                {
                    return Err(invalid("Chat and Responses reasoning uses with_reasoning_effort"));
                }
                if api == OpenCodeApi::ChatCompletions {
                    Arc::new(
                        OpenAICompatible::new_with_reasoning_effort(
                            OpenAICompatibleConfig::new(config.api_key, config.model)
                                .with_base_url(base_url)
                                .with_provider_name(config.service.provider()),
                            config.reasoning_effort,
                        )?
                        .with_reasoning_replay(true)
                        .with_default_headers(headers.clone())?
                        .with_retry_config(config.retry),
                    )
                } else {
                    let native = OpenAIResponsesConfig::new(config.api_key, config.model)
                        .with_base_url(base_url)
                        .with_open_responses_mode(true);
                    let client = match config.reasoning_effort {
                        Some(effort) => {
                            OpenAIResponsesClient::new_with_reasoning_effort(native, effort)
                        }
                        None => OpenAIResponsesClient::new(native),
                    }?;
                    Arc::new(
                        client
                            .with_default_headers(headers.clone())?
                            .with_retry_config(config.retry),
                    )
                }
            }
            OpenCodeApi::Messages => {
                if config.reasoning_effort.is_some() || config.gemini_thinking.is_some() {
                    return Err(invalid(
                        "Messages reasoning uses with_anthropic_effort or with_anthropic_thinking",
                    ));
                }
                let mut native = AnthropicConfig::new(config.api_key, config.model)
                    .with_base_url(base_url.strip_suffix("/v1").expect("validated /v1 suffix"));
                if let Some(thinking) = config.thinking {
                    native = native.with_thinking_mode(thinking);
                }
                if let Some(effort) = config.anthropic_effort {
                    native = native.with_effort(effort);
                }
                Arc::new(
                    AnthropicClient::new(native)?
                        .with_default_headers(headers)
                        .with_retry_config(config.retry),
                )
            }
            OpenCodeApi::GenerateContent => {
                if config.reasoning_effort.is_some()
                    || config.thinking.is_some()
                    || config.anthropic_effort.is_some()
                {
                    return Err(invalid("GenerateContent reasoning uses with_gemini_thinking"));
                }
                let client = adk_gemini::GeminiBuilder::new(config.api_key)
                    .with_model(adk_gemini::Model::Custom(format!("models/{}", config.model)))
                    .with_base_url(
                        reqwest::Url::parse(&format!("{base_url}/")).expect("validated URL"),
                    )
                    .with_http_client(reqwest::Client::builder().default_headers(headers))
                    .build()
                    .map_err(|_| invalid("failed to configure GenerateContent client"))?;
                let mut model =
                    GeminiModel::from_client(client, config.model).with_retry_config(config.retry);
                if let Some(thinking) = config.gemini_thinking {
                    model = model.with_thinking_config(thinking);
                }
                Arc::new(model)
            }
        };
        Ok(Self { inner, api })
    }

    /// Returns the selected wire API.
    pub fn api(&self) -> OpenCodeApi {
        self.api
    }
}

#[async_trait]
impl Llm for OpenCodeClient {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        self.inner.generate_content(request, stream).await
    }
}

fn invalid(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.opencode.invalid_config",
        message,
    )
    .with_provider("opencode")
}
