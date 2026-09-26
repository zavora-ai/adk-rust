use super::OpenCodeApi;

/// OpenCode service to use. Go and Zen have separate endpoints and model routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeService {
    /// Subscription-based coding service.
    Go,
    /// Pay-as-you-go model gateway.
    Zen,
}

impl OpenCodeService {
    /// Official base URL, including the API version.
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Go => "https://opencode.ai/zen/go/v1",
            Self::Zen => "https://opencode.ai/zen/v1",
        }
    }

    pub(super) fn provider(self) -> &'static str {
        match self {
            Self::Go => "opencode-go",
            Self::Zen => "opencode",
        }
    }

    /// API from the published endpoint table, or `None` for an unknown model.
    ///
    /// Sources: <https://opencode.ai/docs/go/#endpoints> and
    /// <https://opencode.ai/docs/zen/#endpoints>. Unknown models require
    /// [`super::OpenCodeConfig::with_api`]. Jev's System One API is not a text model.
    pub fn api(self, model: &str) -> Option<OpenCodeApi> {
        match self {
            Self::Go => Self::go_api(model),
            Self::Zen => Self::zen_api(model),
        }
    }
}

impl OpenCodeService {
    fn go_api(model: &str) -> Option<OpenCodeApi> {
        match model {
            "grok-4.7"
            | "grok-4.6"
            | "gpt-6-luna"
            | "gpt-5.6-luna"
            | "muse-spark-1.3-contributor"
            | "muse-spark-1.2-contributor" => Some(OpenCodeApi::Responses),
            "minimax-m3" | "minimax-m2.7" | "minimax-m2.5" | "qwen3.8-max" | "qwen3.8-flash"
            | "qwen3.7-max" | "qwen3.7-plus" | "qwen3.6-plus" => Some(OpenCodeApi::Messages),
            "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "glm-5.1"
            | "kimi-k3"
            | "kimi-k2.7-code"
            | "kimi-k2.6"
            | "longcat-2.0"
            | "deepseek-v4.1-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "mimo-v2.6-flash"
            | "mimo-v2.6-pro"
            | "mimo-v2.5"
            | "mimo-v2.5-pro"
            | "hy4-preview"
            | "hy3"
            | "space-bunny-free"
            | "longcat-2.5-preview-free" => Some(OpenCodeApi::ChatCompletions),
            _ => None,
        }
    }
}

impl OpenCodeService {
    fn zen_api(model: &str) -> Option<OpenCodeApi> {
        match model {
            "gpt-6-astra"
            | "gpt-6-sol"
            | "gpt-6-luna"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-5.5"
            | "gpt-5.5-pro"
            | "gpt-5.4"
            | "gpt-5.4-pro"
            | "gpt-5.4-mini"
            | "gpt-5.4-nano"
            | "gpt-5.3-codex"
            | "gpt-5.3-codex-spark"
            | "gpt-5.2"
            | "gpt-5.2-codex"
            | "gpt-5.1"
            | "gpt-5.1-codex"
            | "gpt-5.1-codex-max"
            | "gpt-5.1-codex-mini"
            | "gpt-5"
            | "gpt-5-codex"
            | "gpt-5-nano"
            | "grok-4.7"
            | "grok-4.6"
            | "grok-4.5"
            | "grok-build-0.1"
            | "muse-spark-1.3"
            | "muse-spark-1.2"
            | "muse-spark-1.3-contributor-free" => Some(OpenCodeApi::Responses),
            "claude-fable-5-1" | "claude-fable-5" | "claude-opus-5-5" | "claude-opus-5"
            | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" | "claude-opus-4-5"
            | "claude-sonnet-5" | "claude-sonnet-4-6" | "claude-sonnet-4-5"
            | "claude-haiku-4-5" | "qwen3.8-flash" | "qwen3.7-max" | "qwen3.7-plus"
            | "qwen3.6-plus" | "qwen3.5-plus" => Some(OpenCodeApi::Messages),
            "gemini-3.8-flash"
            | "gemini-3.7-flash"
            | "gemini-3.6-flash"
            | "gemini-3.5-flash"
            | "gemini-3.5-flash-lite"
            | "gemini-3.1-pro"
            | "gemini-3-flash" => Some(OpenCodeApi::GenerateContent),
            "qwen3.8-max"
            | "deepseek-v4.1-flash"
            | "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "minimax-m3"
            | "minimax-m2.7"
            | "minimax-m2.5"
            | "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "glm-5.1"
            | "glm-5"
            | "kimi-k2.5"
            | "kimi-k2.6"
            | "kimi-k2.7-code"
            | "kimi-k3"
            | "big-pickle"
            | "space-bunny-free"
            | "longcat-2.5-preview-free"
            | "mimo-v2.6-flash-free"
            | "mimo-v2.5-free"
            | "ling-3.0-flash-fin-free"
            | "nemotron-3-ultra-free"
            | "nemotron-3.5-lightning-free" => Some(OpenCodeApi::ChatCompletions),
            _ => None,
        }
    }
}
