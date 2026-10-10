//! OpenAI client implementation.

use super::config::{OpenAIConfig, OpenAIReasoningEffort, OpenAIResponsesConfig};
use super::responses_client::OpenAIResponsesClient;
use super::schema_adapter::OpenAiSchemaAdapter;
use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use crate::retry::RetryConfig;
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Llm, LlmRequest, LlmResponseStream, SchemaAdapter,
};
use async_trait::async_trait;

/// OpenAI client for standard OpenAI API and OpenAI-compatible APIs.
///
/// Requests go to Chat Completions (`/chat/completions`). OpenAI's Chat Completions
/// endpoint rejects function tools on GPT-5.6 and later models unless reasoning is
/// disabled, so a client built with [`OpenAIClient::new`] for one of those models
/// sends every request that declares tools through the Responses API
/// (`/responses`) instead, with the same credentials, base URL, reasoning effort,
/// and retry policy. Routed requests set `store: false`, matching the Chat
/// Completions default of not storing responses. Requests without tools, models
/// before GPT-5.6, reasoning effort [`OpenAIReasoningEffort::None`], and clients
/// built with [`OpenAIClient::compatible`] stay on Chat Completions.
///
/// # Example
///
/// ```
/// use adk_model::openai::{OpenAIClient, OpenAIConfig};
///
/// // Tool-carrying requests for this model use the Responses API automatically.
/// let client = OpenAIClient::new(OpenAIConfig::new("sk-key", "gpt-5.6-terra"))?;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct OpenAIClient {
    inner: OpenAICompatible,
    /// Responses API client for requests that declare tools, present only when the
    /// model's Chat Completions endpoint rejects function tools with reasoning on.
    tool_route: Option<OpenAIResponsesClient>,
}

/// Returns whether OpenAI Chat Completions rejects function tools for `model` while
/// reasoning is enabled, which applies to GPT-5.6 and every later GPT generation.
///
/// The API answers such a request with HTTP 400: "Function tools with
/// reasoning_effort are not supported for <model> in /v1/chat/completions".
fn chat_rejects_reasoning_tools(model: &str) -> bool {
    let model = model.strip_prefix("ft:").unwrap_or(model);
    let Some(version) = model.strip_prefix("gpt-") else {
        return false;
    };
    let version_end =
        version.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(version.len());
    let mut numbers = version[..version_end].split('.').map(str::parse::<u32>);
    match (numbers.next(), numbers.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (5, 6),
        (Some(Ok(major)), None) => major >= 6,
        _ => false,
    }
}

impl OpenAIClient {
    /// Create a new OpenAI client.
    pub fn new(config: OpenAIConfig) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("openai", &config.model);
        let reasoning_effort = config.reasoning_effort.map(OpenAIReasoningEffort::from);

        Self::new_inner(config, reasoning_effort)
    }

    /// Create a client with the complete OpenAI reasoning-effort vocabulary.
    ///
    /// Use this constructor for newer Chat Completions values such as
    /// [`OpenAIReasoningEffort::None`] or [`OpenAIReasoningEffort::XHigh`].
    /// [`OpenAIReasoningEffort::Max`] is supported by the Responses API, not
    /// Chat Completions. The original [`OpenAIConfig::reasoning_effort`] field
    /// remains available for backward compatibility.
    pub fn new_with_reasoning_effort(
        config: OpenAIConfig,
        reasoning_effort: OpenAIReasoningEffort,
    ) -> Result<Self, AdkError> {
        crate::catalog::warn_if_obsolete("openai", &config.model);
        if reasoning_effort == OpenAIReasoningEffort::Max {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openai.reasoning_effort_unsupported",
                "OpenAI Chat Completions does not support reasoning effort `max`; use OpenAIResponsesClient for `max`, or use `xhigh` with OpenAIClient",
            )
            .with_provider("openai"));
        }
        Self::new_inner(config, Some(reasoning_effort))
    }

    fn new_inner(
        config: OpenAIConfig,
        reasoning_effort: Option<OpenAIReasoningEffort>,
    ) -> Result<Self, AdkError> {
        // An empty key cannot build the Responses client; the request then fails
        // authentication on Chat Completions exactly as before.
        let tool_route = if chat_rejects_reasoning_tools(&config.model)
            && reasoning_effort != Some(OpenAIReasoningEffort::None)
            && !config.api_key.is_empty()
        {
            let mut responses_config =
                OpenAIResponsesConfig::new(config.api_key.clone(), config.model.clone());
            responses_config.organization_id.clone_from(&config.organization_id);
            responses_config.project_id.clone_from(&config.project_id);
            responses_config.base_url.clone_from(&config.base_url);
            let client = match reasoning_effort {
                Some(effort) => {
                    OpenAIResponsesClient::new_with_reasoning_effort(responses_config, effort)?
                }
                None => OpenAIResponsesClient::new(responses_config)?,
            };
            // Chat Completions does not store responses by default; keep that.
            let store_off: super::RequestAdapter = std::sync::Arc::new(|body, _headers| {
                if let Some(body) = body.as_object_mut() {
                    body.entry("store").or_insert(serde_json::Value::Bool(false));
                }
                Ok(())
            });
            Some(client.with_request_adapter(store_off))
        } else {
            None
        };
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
        Ok(Self {
            inner: OpenAICompatible::new_with_reasoning_effort(compat_config, reasoning_effort)?,
            tool_route,
        })
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
        Ok(Self { inner: OpenAICompatible::new(config)?, tool_route: None })
    }

    /// Set the retry configuration (builder pattern).
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.tool_route =
            self.tool_route.map(|route| route.with_retry_config(retry_config.clone()));
        self.inner = self.inner.with_retry_config(retry_config);
        self
    }

    /// Set the retry configuration (mutable reference).
    pub fn set_retry_config(&mut self, retry_config: RetryConfig) {
        if let Some(route) = &mut self.tool_route {
            route.set_retry_config(retry_config.clone());
        }
        self.inner.set_retry_config(retry_config);
    }

    /// Returns the current retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        self.inner.retry_config()
    }
}

#[async_trait]
impl Llm for OpenAIClient {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        static ADAPTER: OpenAiSchemaAdapter = OpenAiSchemaAdapter;
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
    ) -> Result<LlmResponseStream, AdkError> {
        // A request that disables reasoning through the `openai` extension is accepted
        // by Chat Completions, so it keeps the endpoint the caller configured.
        let reasoning_disabled = request
            .config
            .as_ref()
            .and_then(|config| config.extensions.get("openai"))
            .and_then(|openai| openai.get("reasoning_effort"))
            .and_then(serde_json::Value::as_str)
            == Some("none");
        if let Some(route) = &self.tool_route
            && !request.tools.is_empty()
            && !reasoning_disabled
        {
            tracing::debug!(
                model.name = %self.name(),
                "chat completions rejects function tools for this model; using the responses api"
            );
            return route.generate_content(request, stream).await;
        }
        self.inner.generate_content(request, stream).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_rejects_responses_only_max_effort() {
        let config = OpenAIConfig::new("test-key", "gpt-5.6-terra");
        let error =
            match OpenAIClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max) {
                Ok(_) => panic!("Chat Completions must reject max reasoning effort"),
                Err(error) => error,
            };

        assert_eq!(error.code, "model.openai.reasoning_effort_unsupported");
        assert!(error.to_string().contains("OpenAIResponsesClient"));
    }

    #[test]
    fn gpt_5_6_and_later_need_the_responses_api_for_reasoning_tools() {
        let rejecting = [
            "gpt-5.6",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.6-sol",
            "gpt-5.10",
            "gpt-6-astra",
            "gpt-6-luna",
            "gpt-6.1-sol",
            "ft:gpt-5.6-terra:acme::abc123",
        ];
        let accepting = [
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5",
            "gpt-5-mini",
            "gpt-4.1",
            "gpt-4o-mini",
            "gpt-oss-120b",
            "gpt-realtime-2.1",
            "o4-mini",
            "llama-3.3-70b",
        ];
        assert_eq!(
            rejecting.map(chat_rejects_reasoning_tools),
            [true; 9],
            "models that reject Chat Completions function tools: {rejecting:?}"
        );
        assert_eq!(
            accepting.map(chat_rejects_reasoning_tools),
            [false; 10],
            "models that accept Chat Completions function tools: {accepting:?}"
        );
    }

    #[test]
    fn only_affected_openai_clients_build_a_responses_route() {
        let routed = |config: OpenAIConfig| OpenAIClient::new(config).unwrap().tool_route.is_some();
        assert!(routed(OpenAIConfig::new("test-key", crate::catalog::OPENAI_DEFAULT)));
        assert!(!routed(OpenAIConfig::new("test-key", "gpt-5.5")));
        assert!(!routed(OpenAIConfig::new("", "gpt-5.6-terra")));
        let disabled = OpenAIClient::new_with_reasoning_effort(
            OpenAIConfig::new("test-key", "gpt-5.6-terra"),
            OpenAIReasoningEffort::None,
        )
        .unwrap();
        assert!(disabled.tool_route.is_none());
        let compatible =
            OpenAIClient::compatible("test-key", "http://localhost:8000/v1", "gpt-6-astra")
                .unwrap();
        assert!(compatible.tool_route.is_none());
    }
}
