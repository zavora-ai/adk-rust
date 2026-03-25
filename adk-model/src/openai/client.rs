//! OpenAI client implementation.

use super::config::{AzureConfig, OpenAIConfig};
use super::convert;
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig, build_request_json};
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponseStream};
use async_openai::types::chat::ReasoningEffort;
use async_stream::try_stream;
use async_trait::async_trait;

/// OpenAI client for standard OpenAI API and OpenAI-compatible APIs.
pub struct OpenAIClient {
    inner: OpenAICompatible,
}

impl OpenAIClient {
    /// Create a new OpenAI client.
    pub fn new(config: OpenAIConfig) -> Result<Self, AdkError> {
        let reasoning_effort = config.reasoning_effort.map(|e| match e {
            super::config::ReasoningEffort::Low => ReasoningEffort::Low,
            super::config::ReasoningEffort::Medium => ReasoningEffort::Medium,
            super::config::ReasoningEffort::High => ReasoningEffort::High,
        });

        let mut compat_config =
            OpenAICompatibleConfig::new(config.api_key, config.model).with_provider_name("openai");
        if let Some(base_url) = config.base_url {
            compat_config = compat_config.with_base_url(base_url);
        }
        if let Some(org_id) = config.organization_id {
            compat_config = compat_config.with_organization(org_id);
        }
        if let Some(project_id) = config.project_id {
            compat_config = compat_config.with_project(project_id);
        }
        if let Some(effort) = reasoning_effort {
            compat_config = compat_config.with_reasoning_effort(effort);
        }

        Ok(Self { inner: OpenAICompatible::new(compat_config)? })
    }

    /// Create a client for an OpenAI-compatible API.
    pub fn compatible(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, AdkError> {
        let config = OpenAICompatibleConfig::new(api_key, model)
            .with_provider_name("openai-compatible")
            .with_base_url(base_url);
        Ok(Self { inner: OpenAICompatible::new(config)? })
    }

    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.inner = self.inner.with_retry_config(retry_config);
        self
    }

    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        self.inner.set_retry_config(retry_config);
    }

    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for OpenAIClient {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream, AdkError> {
        self.inner.generate_content(request, stream).await
    }
}

/// Azure OpenAI client.
pub struct AzureOpenAIClient {
    http: reqwest::Client,
    api_key: String,
    api_base: String,
    api_version: String,
    deployment_id: String,
    retry_config: RetryConfig,
}

impl AzureOpenAIClient {
    /// Create a new Azure OpenAI client.
    pub fn new(config: AzureConfig) -> Result<Self, AdkError> {
        Ok(Self {
            http: reqwest::Client::new(),
            api_key: config.api_key,
            api_base: config.api_base,
            api_version: config.api_version,
            deployment_id: config.deployment_id,
            retry_config: RetryConfig::default(),
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
impl Llm for AzureOpenAIClient {
    fn name(&self) -> &str {
        &self.deployment_id
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool, // Azure OpenAI always uses non-streaming internally
    ) -> Result<adk_core::LlmResponseStream, AdkError> {
        let usage_span =
            adk_telemetry::llm_generate_span("azure-openai", &self.deployment_id, _stream);
        let deployment_id = self.deployment_id.clone();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let api_base = self.api_base.clone();
        let api_version = self.api_version.clone();
        let retry_config = self.retry_config.clone();
        let request_for_retry = request.clone();

        let stream = try_stream! {
            let response = execute_with_retry(&retry_config, is_retryable_model_error, || {
                let deployment_id = deployment_id.clone();
                let http = http.clone();
                let api_key = api_key.clone();
                let api_base = api_base.clone();
                let api_version = api_version.clone();
                let request = request_for_retry.clone();
                async move {
                    let body = build_request_json(&deployment_id, &request, &None)?;

                    let url = format!(
                        "{api_base}/openai/deployments/{deployment_id}/chat/completions?api-version={api_version}"
                    );

                    let http_resp = http
                        .post(&url)
                        .header("api-key", &api_key)
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| {
                            AdkError::new(
                                ErrorComponent::Model,
                                ErrorCategory::Unavailable,
                                "model.azure_openai.request",
                                format!("Azure OpenAI request error: {e}"),
                            )
                            .with_provider("azure-openai")
                        })?;

                    if !http_resp.status().is_success() {
                        let status_code = http_resp.status().as_u16();
                        let body_text = http_resp.text().await.unwrap_or_default();
                        let msg = format!("Azure OpenAI API error (HTTP {status_code}): {body_text}");
                        let (category, code, status) = match status_code {
                            429 => (ErrorCategory::RateLimited, "model.azure_openai.rate_limited", Some(429u16)),
                            503 => (ErrorCategory::Unavailable, "model.azure_openai.unavailable", Some(503u16)),
                            529 => (ErrorCategory::Unavailable, "model.azure_openai.overloaded", Some(529u16)),
                            408 => (ErrorCategory::Timeout, "model.azure_openai.timeout", Some(408u16)),
                            401 => (ErrorCategory::Unauthorized, "model.azure_openai.unauthorized", Some(401u16)),
                            404 => (ErrorCategory::NotFound, "model.azure_openai.not_found", Some(404u16)),
                            _ if status_code >= 500 => (ErrorCategory::Internal, "model.azure_openai.api_error", Some(status_code)),
                            _ => (ErrorCategory::Internal, "model.azure_openai.api_error", Some(status_code)),
                        };
                        let mut err = AdkError::new(ErrorComponent::Model, category, code, msg)
                            .with_provider("azure-openai");
                        if let Some(sc) = status {
                            err = err.with_upstream_status(sc);
                        }
                        return Err(err);
                    }

                    let raw_json: serde_json::Value = http_resp.json().await.map_err(|e| {
                        AdkError::new(
                            ErrorComponent::Model,
                            ErrorCategory::Internal,
                            "model.azure_openai.parse",
                            format!("Azure OpenAI response parse error: {e}"),
                        )
                        .with_provider("azure-openai")
                    })?;

                    tracing::debug!(
                        provider = "azure-openai",
                        model = %deployment_id,
                        has_reasoning = raw_json
                            .pointer("/choices/0/message/reasoning_content")
                            .is_some(),
                        "azure openai chat completion response"
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
