use super::OpenCodeService;
use crate::anthropic::{Effort, ThinkingMode};
use crate::gemini::ThinkingConfig;
use crate::openai::OpenAIReasoningEffort;
use crate::retry::RetryConfig;

/// Wire API accepted by an OpenCode model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeApi {
    /// OpenAI-compatible Chat Completions.
    ChatCompletions,
    /// OpenAI Responses.
    Responses,
    /// Anthropic Messages.
    Messages,
    /// Google Gemini GenerateContent.
    GenerateContent,
}

/// Configuration for one OpenCode model and conversation.
///
/// Set the application's own user agent and reuse the session ID for all main
/// and auxiliary requests in the same conversation. Credentials are omitted
/// from the debug representation.
///
/// # Example
///
/// ```
/// use adk_model::opencode::{OpenCodeConfig, OpenCodeService};
/// let config = OpenCodeConfig::new(OpenCodeService::Go, "api-key", "deepseek-v4.1-flash")
///     .with_user_agent("example-coding-agent/1.0")
///     .with_session_id("conversation-42");
/// ```
#[derive(Clone)]
pub struct OpenCodeConfig {
    pub(super) service: OpenCodeService,
    pub(super) gemini_thinking: Option<ThinkingConfig>,
    pub(super) api_key: String,
    pub(super) model: String,
    pub(super) base_url: String,
    pub(super) api: Option<OpenCodeApi>,
    pub(super) user_agent: String,
    pub(super) session_id: String,
    pub(super) reasoning_effort: Option<OpenAIReasoningEffort>,
    pub(super) thinking: Option<ThinkingMode>,
    pub(super) anthropic_effort: Option<Effort>,
    pub(super) retry: RetryConfig,
}

impl std::fmt::Debug for OpenCodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeConfig")
            .field("service", &self.service)
            .field("model", &self.model)
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

impl OpenCodeConfig {
    /// Creates a config with the selected service's official URL and model routing.
    pub fn new(
        service: OpenCodeService,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            service,
            gemini_thinking: None,
            api_key: api_key.into(),
            model: model.into(),
            base_url: service.base_url().into(),
            api: None,
            user_agent: String::new(),
            session_id: String::new(),
            reasoning_effort: None,
            thinking: None,
            anthropic_effort: None,
            retry: RetryConfig::default(),
        }
    }

    /// Sets the API explicitly, including for models added after this library release.
    pub fn with_api(mut self, api: OpenCodeApi) -> Self {
        self.api = Some(api);
        self
    }

    /// Sets a proxy or test endpoint including its `/v1` suffix.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Identifies the calling coding application, such as `my-coding-agent/1.0`.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Sets the stable conversation ID sent as `x-opencode-session`.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    /// Sets reasoning effort for Chat Completions or Responses models.
    ///
    /// Messages models use [`Self::with_anthropic_effort`] instead.
    pub fn with_reasoning_effort(mut self, effort: OpenAIReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Sets thinking for models using Anthropic Messages.
    pub fn with_anthropic_thinking(mut self, thinking: ThinkingMode) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Sets output effort for models using Anthropic Messages.
    pub fn with_anthropic_effort(mut self, effort: Effort) -> Self {
        self.anthropic_effort = Some(effort);
        self
    }

    /// Sets thinking for Gemini GenerateContent models.
    pub fn with_gemini_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.gemini_thinking = Some(thinking);
        self
    }

    /// Sets the underlying model client's retry policy.
    pub fn with_retry_config(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }
}
