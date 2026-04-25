//! Configuration types for DeepSeek provider.
//!
//! Supports DeepSeek V4 models (`deepseek-v4-pro`, `deepseek-v4-flash`) and
//! legacy models (`deepseek-chat`, `deepseek-reasoner`).

use serde::{Deserialize, Serialize};

/// Default DeepSeek API base URL.
pub const DEEPSEEK_API_BASE: &str = "https://api.deepseek.com";

/// DeepSeek beta API base URL (for FIM completion, prefix completion, strict tool mode).
pub const DEEPSEEK_BETA_API_BASE: &str = "https://api.deepseek.com/beta";

/// DeepSeek Anthropic-compatible API base URL.
pub const DEEPSEEK_ANTHROPIC_API_BASE: &str = "https://api.deepseek.com/anthropic";

/// Thinking mode toggle for DeepSeek V4 models.
///
/// V4 models default to thinking enabled. Use `Disabled` to explicitly turn it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingMode {
    /// Enable chain-of-thought reasoning before the final answer.
    Enabled,
    /// Disable thinking mode (no reasoning output).
    Disabled,
}

/// Reasoning effort level for thinking mode.
///
/// Controls how much computation the model spends on chain-of-thought reasoning.
/// V4 defaults to `High` for regular requests; complex agent requests auto-set `Max`.
///
/// For compatibility, `Low` and `Medium` are mapped to `High` by the API,
/// and `XHigh` is mapped to `Max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Standard reasoning depth (default for regular requests).
    High,
    /// Maximum reasoning depth (default for complex agent requests).
    Max,
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Max => write!(f, "max"),
        }
    }
}

/// Configuration for DeepSeek API.
///
/// # V4 Models
///
/// ```rust,ignore
/// use adk_model::deepseek::DeepSeekConfig;
///
/// // V4 Pro (strongest reasoning)
/// let pro = DeepSeekConfig::v4_pro("api-key");
///
/// // V4 Flash (fast, cost-efficient)
/// let flash = DeepSeekConfig::v4_flash("api-key");
///
/// // V4 Pro with max reasoning effort
/// let pro_max = DeepSeekConfig::v4_pro("api-key")
///     .with_reasoning_effort(ReasoningEffort::Max);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekConfig {
    /// DeepSeek API key.
    pub api_key: String,
    /// Model name (e.g., `"deepseek-v4-pro"`, `"deepseek-v4-flash"`, `"deepseek-chat"`).
    pub model: String,
    /// Optional custom base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Thinking mode toggle. `None` = server default (enabled for V4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMode>,
    /// Reasoning effort level. `None` = server default (`high`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Maximum tokens for output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Enable beta features (prefix completion, FIM, strict tool mode).
    /// When true, uses `https://api.deepseek.com/beta` as base URL.
    #[serde(default)]
    pub beta: bool,
    /// Enable strict tool mode (beta). When true, tool definitions include
    /// `"strict": true` and the model strictly follows the JSON schema.
    #[serde(default)]
    pub strict_tools: bool,

    // --- Backward compatibility ---
    /// Legacy field: `true` maps to `thinking: Some(Enabled)`.
    /// Prefer using `thinking` directly for new code.
    #[serde(default)]
    pub thinking_enabled: bool,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "deepseek-v4-flash".to_string(),
            base_url: None,
            thinking: None,
            reasoning_effort: None,
            max_tokens: None,
            beta: false,
            strict_tools: false,
            thinking_enabled: false,
        }
    }
}

impl DeepSeekConfig {
    /// Create a new DeepSeek config with the given API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), model: model.into(), ..Default::default() }
    }

    // --- V4 model constructors ---

    /// Create a config for `deepseek-v4-pro` (strongest reasoning, thinking enabled).
    pub fn v4_pro(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "deepseek-v4-pro".to_string(),
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(ReasoningEffort::High),
            max_tokens: Some(8192),
            ..Default::default()
        }
    }

    /// Create a config for `deepseek-v4-flash` (fast, cost-efficient).
    pub fn v4_flash(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "deepseek-v4-flash".to_string(),
            ..Default::default()
        }
    }

    // --- Legacy model constructors (backward compatible) ---

    /// Create a config for `deepseek-chat` model.
    pub fn chat(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "deepseek-chat")
    }

    /// Create a config for `deepseek-reasoner` model with thinking enabled.
    pub fn reasoner(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "deepseek-reasoner".to_string(),
            thinking: Some(ThinkingMode::Enabled),
            thinking_enabled: true,
            max_tokens: Some(8192),
            ..Default::default()
        }
    }

    // --- Builder methods ---

    /// Set thinking mode explicitly.
    pub fn with_thinking_mode(mut self, mode: ThinkingMode) -> Self {
        self.thinking = Some(mode);
        if mode == ThinkingMode::Enabled {
            self.thinking_enabled = true;
        }
        self
    }

    /// Enable or disable thinking mode (legacy API, prefer `with_thinking_mode`).
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self.thinking = Some(if enabled { ThinkingMode::Enabled } else { ThinkingMode::Disabled });
        self
    }

    /// Set reasoning effort level for thinking mode.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Set max tokens for output.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set custom base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Enable beta features (prefix completion, FIM, strict tool mode).
    pub fn with_beta(mut self) -> Self {
        self.beta = true;
        self
    }

    /// Enable strict tool mode (beta feature).
    /// Tool definitions will include `"strict": true` and the model will
    /// strictly follow the JSON schema for tool call arguments.
    pub fn with_strict_tools(mut self) -> Self {
        self.strict_tools = true;
        self.beta = true; // strict tools require beta
        self
    }

    /// Get the effective base URL.
    pub fn effective_base_url(&self) -> &str {
        if let Some(ref url) = self.base_url {
            return url;
        }
        if self.beta {
            return DEEPSEEK_BETA_API_BASE;
        }
        DEEPSEEK_API_BASE
    }

    /// Whether thinking mode is effectively enabled.
    pub fn is_thinking_enabled(&self) -> bool {
        match self.thinking {
            Some(ThinkingMode::Enabled) => true,
            Some(ThinkingMode::Disabled) => false,
            None => self.thinking_enabled, // legacy fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_is_v4_flash() {
        let config = DeepSeekConfig::default();
        assert_eq!(config.model, "deepseek-v4-flash");
    }

    #[test]
    fn test_v4_pro_constructor() {
        let config = DeepSeekConfig::v4_pro("key");
        assert_eq!(config.model, "deepseek-v4-pro");
        assert_eq!(config.thinking, Some(ThinkingMode::Enabled));
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::High));
        assert!(config.is_thinking_enabled());
    }

    #[test]
    fn test_v4_flash_constructor() {
        let config = DeepSeekConfig::v4_flash("key");
        assert_eq!(config.model, "deepseek-v4-flash");
        assert!(config.thinking.is_none());
    }

    #[test]
    fn test_legacy_chat_constructor() {
        let config = DeepSeekConfig::chat("key");
        assert_eq!(config.model, "deepseek-chat");
        assert!(!config.is_thinking_enabled());
    }

    #[test]
    fn test_legacy_reasoner_constructor() {
        let config = DeepSeekConfig::reasoner("key");
        assert_eq!(config.model, "deepseek-reasoner");
        assert!(config.is_thinking_enabled());
    }

    #[test]
    fn test_with_reasoning_effort() {
        let config = DeepSeekConfig::v4_pro("key").with_reasoning_effort(ReasoningEffort::Max);
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Max));
    }

    #[test]
    fn test_with_thinking_mode_disabled() {
        let config = DeepSeekConfig::v4_pro("key").with_thinking_mode(ThinkingMode::Disabled);
        assert!(!config.is_thinking_enabled());
    }

    #[test]
    fn test_beta_base_url() {
        let config = DeepSeekConfig::v4_pro("key").with_beta();
        assert_eq!(config.effective_base_url(), DEEPSEEK_BETA_API_BASE);
    }

    #[test]
    fn test_strict_tools_enables_beta() {
        let config = DeepSeekConfig::v4_pro("key").with_strict_tools();
        assert!(config.beta);
        assert!(config.strict_tools);
        assert_eq!(config.effective_base_url(), DEEPSEEK_BETA_API_BASE);
    }

    #[test]
    fn test_custom_base_url_overrides_beta() {
        let config =
            DeepSeekConfig::v4_pro("key").with_beta().with_base_url("https://custom.api.com");
        assert_eq!(config.effective_base_url(), "https://custom.api.com");
    }

    #[test]
    fn test_legacy_with_thinking_true() {
        let config = DeepSeekConfig::chat("key").with_thinking(true);
        assert!(config.is_thinking_enabled());
        assert_eq!(config.thinking, Some(ThinkingMode::Enabled));
    }

    #[test]
    fn test_legacy_with_thinking_false() {
        let config = DeepSeekConfig::reasoner("key").with_thinking(false);
        assert!(!config.is_thinking_enabled());
        assert_eq!(config.thinking, Some(ThinkingMode::Disabled));
    }

    #[test]
    fn test_reasoning_effort_display() {
        assert_eq!(ReasoningEffort::High.to_string(), "high");
        assert_eq!(ReasoningEffort::Max.to_string(), "max");
    }
}
