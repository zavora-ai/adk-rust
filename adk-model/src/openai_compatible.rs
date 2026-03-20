//! Shared OpenAI-compatible provider implementation.

use crate::openai::convert;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, Llm, LlmRequest, LlmResponseStream};
use async_openai::types::chat::{
    CreateChatCompletionRequestArgs, ReasoningEffort, ResponseFormat, ResponseFormatJsonSchema,
};
use async_stream::try_stream;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Configuration for OpenAI-compatible providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAICompatibleConfig {
    /// Provider display name used in error messages.
    pub provider_name: String,
    /// API key.
    pub api_key: String,
    /// Model name.
    pub model: String,
    /// Optional API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional organization ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Optional project ID for providers that support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Optional reasoning effort for OpenAI reasoning models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl OpenAICompatibleConfig {
    /// Create config for an OpenAI-compatible provider.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "openai-compatible".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: None,
            organization_id: None,
            project_id: None,
            reasoning_effort: None,
        }
    }

    /// Set provider display name used in errors.
    pub fn with_provider_name(mut self, provider_name: impl Into<String>) -> Self {
        self.provider_name = provider_name.into();
        self
    }

    /// Set a custom API base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Set organization ID.
    pub fn with_organization(mut self, organization_id: impl Into<String>) -> Self {
        self.organization_id = Some(organization_id.into());
        self
    }

    /// Set project ID.
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Set reasoning effort for reasoning models.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    // ── Provider presets ─────────────────────────────────────────

    /// Fireworks AI preset.
    ///
    /// Default model: `accounts/fireworks/models/llama-v3p1-8b-instruct`
    pub fn fireworks(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("fireworks")
            .with_base_url("https://api.fireworks.ai/inference/v1")
    }

    /// Together AI preset.
    ///
    /// Default model: `meta-llama/Llama-3.3-70B-Instruct-Turbo`
    pub fn together(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("together")
            .with_base_url("https://api.together.xyz/v1")
    }

    /// Mistral AI preset.
    ///
    /// Default model: `mistral-small-latest`
    pub fn mistral(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("mistral")
            .with_base_url("https://api.mistral.ai/v1")
    }

    /// Perplexity preset.
    ///
    /// Default model: `sonar`
    pub fn perplexity(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("perplexity")
            .with_base_url("https://api.perplexity.ai")
    }

    /// Cerebras preset.
    ///
    /// Default model: `llama-3.3-70b`
    pub fn cerebras(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("cerebras")
            .with_base_url("https://api.cerebras.ai/v1")
    }

    /// SambaNova preset.
    ///
    /// Default model: `Meta-Llama-3.3-70B-Instruct`
    pub fn sambanova(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model)
            .with_provider_name("sambanova")
            .with_base_url("https://api.sambanova.ai/v1")
    }

    /// xAI (Grok) preset.
    ///
    /// Default model: `grok-3-mini`
    pub fn xai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(api_key, model).with_provider_name("xai").with_base_url("https://api.x.ai/v1")
    }
}

/// Shared OpenAI-compatible client implementation.
pub struct OpenAICompatible {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    provider_name: String,
    retry_config: RetryConfig,
    reasoning_effort: Option<ReasoningEffort>,
    organization_id: Option<String>,
}

impl OpenAICompatible {
    /// Create a new OpenAI-compatible client.
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, AdkError> {
        let base_url = config.base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        Ok(Self {
            http: reqwest::Client::new(),
            api_key: config.api_key,
            base_url,
            model: config.model,
            provider_name: config.provider_name,
            retry_config: RetryConfig::default(),
            reasoning_effort: config.reasoning_effort,
            organization_id: config.organization_id,
        })
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
}

#[async_trait]
impl Llm for OpenAICompatible {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        let model = self.model.clone();
        let provider_name = self.provider_name.clone();
        let span_provider = provider_name.clone();
        let span_model = model.clone();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();
        let reasoning_effort = self.reasoning_effort.clone();
        let organization_id = self.organization_id.clone();

        let usage_span = adk_telemetry::llm_generate_span(&span_provider, &span_model, _stream);

        let stream = try_stream! {
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let model = model.clone();
                let provider_name = provider_name.clone();
                let http = http.clone();
                let api_key = api_key.clone();
                let base_url = base_url.clone();
                let request = request_for_retry.clone();
                let reasoning_effort = reasoning_effort.clone();
                let organization_id = organization_id.clone();
                async move {
                    let messages: Vec<_> = request
                        .contents
                        .iter()
                        .map(convert::content_to_message)
                        .collect();

                    let mut request_builder = CreateChatCompletionRequestArgs::default();
                    request_builder.model(&model).messages(messages);

                    if !request.tools.is_empty() {
                        let tools = convert::convert_tools(&request.tools);
                        request_builder.tools(tools);
                    }

                    if let Some(effort) = reasoning_effort {
                        request_builder.reasoning_effort(effort);
                    }

                    if let Some(config) = &request.config {
                        if let Some(temp) = config.temperature {
                            request_builder.temperature(temp);
                        }
                        if let Some(top_p) = config.top_p {
                            request_builder.top_p(top_p);
                        }
                        if let Some(max_tokens) = config.max_output_tokens {
                            request_builder.max_completion_tokens(max_tokens as u32);
                        }

                        if let Some(schema) = &config.response_schema {
                            let mut schema_with_strict = schema.clone();
                            if let Some(obj) = schema_with_strict.as_object_mut() {
                                obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                            }
                            let json_schema = ResponseFormatJsonSchema {
                                name: request.model.replace(['-', '.', '/'], "_"),
                                description: None,
                                schema: Some(schema_with_strict),
                                strict: Some(true),
                            };
                            request_builder.response_format(ResponseFormat::JsonSchema { json_schema });
                        }
                    }

                    let openai_request = request_builder
                        .build()
                        .map_err(|e| AdkError::Model(format!("failed to build request: {e}")))?;

                    // Send via reqwest directly instead of async-openai's client.
                    // This lets us parse the full JSON response including
                    // `reasoning_content` which async-openai 0.33 does not capture.
                    let url = format!("{base_url}/chat/completions");
                    let mut http_req = http
                        .post(&url)
                        .bearer_auth(&api_key)
                        .json(&openai_request);

                    if let Some(org_id) = &organization_id {
                        http_req = http_req.header("OpenAI-Organization", org_id);
                    }

                    let http_resp = http_req
                        .send()
                        .await
                        .map_err(|e| AdkError::Model(format!("{provider_name} request error: {e}")))?;

                    if !http_resp.status().is_success() {
                        let status = http_resp.status();
                        let body = http_resp.text().await.unwrap_or_default();
                        return Err(AdkError::Model(
                            format!("{provider_name} API error (HTTP {status}): {body}")
                        ));
                    }

                    let raw_json: serde_json::Value = http_resp
                        .json()
                        .await
                        .map_err(|e| AdkError::Model(format!("{provider_name} response parse error: {e}")))?;

                    tracing::debug!(
                        provider = %provider_name,
                        model = %model,
                        has_reasoning = raw_json.pointer("/choices/0/message/reasoning_content").is_some(),
                        "openai chat completion response"
                    );

                    Ok(raw_json)
                }
            })
            .await?;

            let adk_response = convert::from_raw_openai_response(&response);
            yield adk_response;
        };

        Ok(crate::usage_tracking::with_usage_tracking(Box::pin(stream), usage_span))
    }
}
