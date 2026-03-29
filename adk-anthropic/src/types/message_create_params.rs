use serde::{Deserialize, Serialize};

use crate::types::{
    CacheControlEphemeral, CitationsConfig, ContextManagement, EffortLevel, MessageParam, Metadata,
    Model, OutputConfig, OutputFormat, SkillRef, SpeedMode, SystemPrompt, TextBlock,
    ThinkingConfig, ToolChoice, ToolUnionParam,
};

/// Security limits for DoS prevention
const MAX_MESSAGE_COUNT: usize = 1000;
const MAX_MESSAGE_LENGTH: usize = 1_000_000; // 1MB per message
const MAX_STOP_SEQUENCES: usize = 100;
const MAX_STOP_SEQUENCE_LENGTH: usize = 1000;
const MAX_SYSTEM_PROMPT_LENGTH: usize = 100_000;
const MAX_TOOLS_COUNT: usize = 100;

/// Parameters for creating messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageCreateParams {
    /// The maximum number of tokens to generate before stopping.
    ///
    /// Note that our models may stop _before_ reaching this maximum. This parameter
    /// only specifies the absolute maximum number of tokens to generate.
    ///
    /// Different models have different maximum values for this parameter. See
    /// [models](https://docs.anthropic.com/en/docs/models-overview) for details.
    pub max_tokens: u32,

    /// Input messages.
    ///
    /// Our models are trained to operate on alternating `user` and `assistant`
    /// conversational turns. When creating a new `Message`, you specify the prior
    /// conversational turns with the `messages` parameter, and the model then generates
    /// the next `Message` in the conversation. Consecutive `user` or `assistant` turns
    /// in your request will be combined into a single turn.
    pub messages: Vec<MessageParam>,

    /// The model that will complete your prompt.
    ///
    /// See [models](https://docs.anthropic.com/en/docs/models-overview) for additional
    /// details and options.
    pub model: Model,

    /// An object describing metadata about the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,

    /// Output format configuration for structured outputs (legacy field).
    ///
    /// Prefer `output_config` for new code. This field is retained for backward
    /// compatibility with existing callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,

    /// Custom text sequences that will cause the model to stop generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    /// System prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,

    /// Amount of randomness injected into the response.
    ///
    /// Defaults to `1.0`. Ranges from `0.0` to `1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Configuration for enabling Claude's extended thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,

    /// How the model should use the provided tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Definitions of tools that the model may use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolUnionParam>>,

    /// Only sample from the top K options for each subsequent token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    /// Use nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Whether to incrementally stream the response using server-sent events.
    pub stream: bool,

    // --- New fields (Anthropic API parity, March 2026) ---
    /// Top-level effort parameter controlling response thoroughness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,

    /// Speed mode for latency-critical workloads (research preview).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<SpeedMode>,

    /// Structured output configuration (replaces `output_format`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,

    /// Server-side context management (tool result clearing, thinking block clearing).
    /// Requires beta header `context-management-2025-06-27`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagement>,

    /// Geographic routing for data residency control (e.g. "US", "EU").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<String>,

    /// Service tier for priority capacity (`"auto"` or `"standard_only"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// Container identifier for code execution tool reuse across requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,

    /// Top-level cache control. When set, the server automatically caches
    /// everything up to the last cacheable block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Citations configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<CitationsConfig>,

    /// Skill references for dynamic instruction loading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<SkillRef>>,

    /// Opt into programmatic tool calling flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_runner: Option<bool>,
}

impl MessageCreateParams {
    /// Helper function to validate a float value is within the 0.0-1.0 range (optimized)
    #[inline]
    fn validate_float_range(value: f32, field_name: &str) -> Result<(), crate::Error> {
        // Fast path for common valid values
        if (0.0..=1.0).contains(&value) && value.is_finite() {
            return Ok(());
        }

        // Handle edge cases
        if value.is_nan() {
            return Err(crate::Error::validation(
                format!("{field_name} cannot be NaN"),
                Some(field_name.to_string()),
            ));
        }

        Err(crate::Error::validation(
            format!("{field_name} must be between 0.0 and 1.0, got {value}"),
            Some(field_name.to_string()),
        ))
    }
    /// Create a new message creation parameters with streaming disabled.
    pub fn new(max_tokens: u32, messages: Vec<MessageParam>, model: Model) -> Self {
        Self {
            max_tokens,
            messages,
            model,
            metadata: None,
            output_format: None,
            stop_sequences: None,
            system: None,
            temperature: None,
            thinking: None,
            tool_choice: None,
            tools: None,
            top_k: None,
            top_p: None,
            stream: false,
            effort: None,
            speed: None,
            output_config: None,

            context_management: None,
            inference_geo: None,
            service_tier: None,
            container: None,
            cache_control: None,
            citations: None,
            skills: None,
            tool_runner: None,
        }
    }

    /// Create new streaming message creation parameters.
    pub fn new_streaming(max_tokens: u32, messages: Vec<MessageParam>, model: Model) -> Self {
        Self {
            max_tokens,
            messages,
            model,
            metadata: None,
            output_format: None,
            stop_sequences: None,
            system: None,
            temperature: None,
            thinking: None,
            tool_choice: None,
            tools: None,
            top_k: None,
            top_p: None,
            stream: true,
            effort: None,
            speed: None,
            output_config: None,

            context_management: None,
            inference_geo: None,
            service_tier: None,
            container: None,
            cache_control: None,
            citations: None,
            skills: None,
            tool_runner: None,
        }
    }

    /// Add metadata to the parameters.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add output format for structured outputs.
    ///
    /// When set, constrains Claude's response to follow a specific JSON schema,
    /// ensuring valid, parseable output for downstream processing.
    ///
    /// This feature requires the beta header `structured-outputs-2025-11-13`.
    ///
    /// # Example
    ///
    /// ```
    /// use serde_json::json;
    /// use adk_anthropic::{MessageCreateParams, OutputFormat, KnownModel};
    ///
    /// let params = MessageCreateParams::simple("Extract info", KnownModel::ClaudeHaiku45)
    ///     .with_output_format(OutputFormat::json_schema(json!({
    ///         "type": "object",
    ///         "properties": {
    ///             "name": { "type": "string" }
    ///         },
    ///         "required": ["name"],
    ///         "additionalProperties": false
    ///     })));
    /// ```
    pub fn with_output_format(mut self, output_format: OutputFormat) -> Self {
        self.output_format = Some(output_format);
        self
    }

    /// Add stop sequences to the parameters.
    pub fn with_stop_sequences(mut self, stop_sequences: Vec<String>) -> Self {
        self.stop_sequences = Some(stop_sequences);
        self
    }

    /// Add a system prompt as a string.
    pub fn with_system_string(mut self, system: String) -> Self {
        self.system = Some(SystemPrompt::from_string(system));
        self
    }

    /// Add a system prompt as text blocks.
    pub fn with_system_blocks(mut self, blocks: Vec<TextBlock>) -> Self {
        self.system = Some(SystemPrompt::from_blocks(blocks));
        self
    }

    /// Add a system prompt.
    pub fn with_system(mut self, system: impl Into<SystemPrompt>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Add temperature to the parameters.
    pub fn with_temperature(mut self, temperature: f32) -> Result<Self, crate::Error> {
        Self::validate_float_range(temperature, "temperature")?;
        self.temperature = Some(temperature);
        Ok(self)
    }

    /// Add thinking configuration to the parameters.
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Add tool choice to the parameters.
    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    /// Add tools to the parameters.
    pub fn with_tools(mut self, tools: Vec<ToolUnionParam>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Add top_k to the parameters.
    pub fn with_top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Add top_p to the parameters.
    pub fn with_top_p(mut self, top_p: f32) -> Result<Self, crate::Error> {
        Self::validate_float_range(top_p, "top_p")?;
        self.top_p = Some(top_p);
        Ok(self)
    }

    /// Sets the streaming option.
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Validate all parameters before sending to the API with security checks.
    ///
    /// Performs comprehensive validation including DoS prevention measures:
    /// - Limits on message count and size to prevent resource exhaustion
    /// - Validation of all numeric ranges
    /// - Security checks on string inputs
    pub fn validate(&self) -> Result<(), crate::Error> {
        // Basic parameter validation
        if self.max_tokens == 0 {
            return Err(crate::Error::validation(
                "max_tokens must be greater than 0",
                Some("max_tokens".to_string()),
            ));
        }

        // Security: Prevent excessive token requests that could be expensive
        if self.max_tokens > 1_000_000 {
            return Err(crate::Error::validation(
                format!("max_tokens exceeds security limit of 1,000,000, got {}", self.max_tokens),
                Some("max_tokens".to_string()),
            ));
        }

        if self.messages.is_empty() {
            return Err(crate::Error::validation(
                "At least one message is required",
                Some("messages".to_string()),
            ));
        }

        // Security: Prevent DoS via excessive message count
        if self.messages.len() > MAX_MESSAGE_COUNT {
            return Err(crate::Error::validation(
                format!(
                    "Message count {} exceeds security limit of {}",
                    self.messages.len(),
                    MAX_MESSAGE_COUNT
                ),
                Some("messages".to_string()),
            ));
        }

        // Validate message content sizes
        for (i, message) in self.messages.iter().enumerate() {
            let content_str = format!("{:?}", message.content); // Rough size estimate
            if content_str.len() > MAX_MESSAGE_LENGTH {
                return Err(crate::Error::validation(
                    format!(
                        "Message {} content size {} exceeds limit of {}",
                        i,
                        content_str.len(),
                        MAX_MESSAGE_LENGTH
                    ),
                    Some(format!("messages[{i}]")),
                ));
            }
        }

        // Validate floating point parameters
        if let Some(temp) = self.temperature {
            Self::validate_float_range(temp, "temperature")?;
        }
        if let Some(top_p) = self.top_p {
            Self::validate_float_range(top_p, "top_p")?;
        }

        // Validate top_k is reasonable
        if let Some(top_k) = self.top_k
            && top_k > 1000
        {
            return Err(crate::Error::validation(
                format!("top_k {top_k} exceeds reasonable limit of 1000"),
                Some("top_k".to_string()),
            ));
        }

        // Validate stop sequences
        if let Some(ref stop_sequences) = self.stop_sequences {
            if stop_sequences.len() > MAX_STOP_SEQUENCES {
                return Err(crate::Error::validation(
                    format!(
                        "Stop sequences count {} exceeds limit of {}",
                        stop_sequences.len(),
                        MAX_STOP_SEQUENCES
                    ),
                    Some("stop_sequences".to_string()),
                ));
            }

            for (i, seq) in stop_sequences.iter().enumerate() {
                if seq.len() > MAX_STOP_SEQUENCE_LENGTH {
                    return Err(crate::Error::validation(
                        format!(
                            "Stop sequence {} length {} exceeds limit of {}",
                            i,
                            seq.len(),
                            MAX_STOP_SEQUENCE_LENGTH
                        ),
                        Some(format!("stop_sequences[{i}]")),
                    ));
                }

                // Security: Check for potentially problematic characters
                if seq.contains('\0') {
                    return Err(crate::Error::validation(
                        format!("Stop sequence {i} contains null bytes"),
                        Some(format!("stop_sequences[{i}]")),
                    ));
                }
            }
        }

        // Validate system prompt size
        if let Some(ref system) = self.system {
            let system_str = format!("{system:?}"); // Rough size estimate
            if system_str.len() > MAX_SYSTEM_PROMPT_LENGTH {
                return Err(crate::Error::validation(
                    format!(
                        "System prompt size {} exceeds limit of {}",
                        system_str.len(),
                        MAX_SYSTEM_PROMPT_LENGTH
                    ),
                    Some("system".to_string()),
                ));
            }
        }

        // Validate tools count
        if let Some(ref tools) = self.tools
            && tools.len() > MAX_TOOLS_COUNT
        {
            return Err(crate::Error::validation(
                format!("Tools count {} exceeds limit of {}", tools.len(), MAX_TOOLS_COUNT),
                Some("tools".to_string()),
            ));
        }

        // Validate thinking config with security checks
        if let Some(ref thinking) = self.thinking {
            match thinking {
                ThinkingConfig::Enabled { budget_tokens, .. } => {
                    if *budget_tokens < 1024 {
                        return Err(crate::Error::validation(
                            format!(
                                "Thinking budget must be at least 1024 tokens, got {budget_tokens}"
                            ),
                            Some("thinking.budget_tokens".to_string()),
                        ));
                    }
                    if *budget_tokens > self.max_tokens {
                        return Err(crate::Error::validation(
                            format!(
                                "Thinking budget ({budget_tokens}) cannot exceed max_tokens ({})",
                                self.max_tokens
                            ),
                            Some("thinking.budget_tokens".to_string()),
                        ));
                    }

                    // Security: Prevent excessive thinking budget
                    if *budget_tokens > 100_000 {
                        return Err(crate::Error::validation(
                            format!(
                                "Thinking budget {budget_tokens} exceeds security limit of 100,000"
                            ),
                            Some("thinking.budget_tokens".to_string()),
                        ));
                    }
                }
                ThinkingConfig::Disabled => {
                    // No validation needed for disabled state
                }
                ThinkingConfig::Adaptive { .. } => {
                    // No validation needed for adaptive thinking
                }
            }
        }

        Ok(())
    }

    /// Create a simple message request with sensible defaults.
    ///
    /// This is a convenience method for creating basic message requests without
    /// needing to specify all parameters explicitly.
    pub fn simple(prompt: impl Into<MessageParam>, model: impl Into<Model>) -> Self {
        Self::new(
            1024, // Reasonable default for max_tokens
            vec![prompt.into()],
            model.into(),
        )
    }

    /// Create a simple streaming message request with sensible defaults.
    pub fn simple_streaming(prompt: impl Into<MessageParam>, model: impl Into<Model>) -> Self {
        Self::new_streaming(
            1024, // Reasonable default for max_tokens
            vec![prompt.into()],
            model.into(),
        )
    }

    /// Add a single message to the parameters.
    pub fn with_message(mut self, message: impl Into<MessageParam>) -> Self {
        self.messages.push(message.into());
        self
    }

    /// Add multiple messages to the parameters.
    pub fn with_messages(
        mut self,
        messages: impl IntoIterator<Item = impl Into<MessageParam>>,
    ) -> Self {
        self.messages.extend(messages.into_iter().map(|m| m.into()));
        self
    }

    /// Check if this request requires the structured outputs beta header.
    ///
    /// Returns `true` if either:
    /// - `output_format` is set (for JSON outputs)
    /// - Any tool has `strict: true` (for strict tool use)
    ///
    /// When this returns `true`, the client should include the
    /// `anthropic-beta: structured-outputs-2025-11-13` header.
    pub fn requires_structured_outputs_beta(&self) -> bool {
        // Check if output_format is set
        if self.output_format.is_some() {
            return true;
        }

        // Check if any tool has strict mode enabled
        if let Some(ref tools) = self.tools {
            for tool in tools {
                if tool.is_strict() {
                    return true;
                }
            }
        }

        false
    }
}

impl Default for MessageCreateParams {
    fn default() -> Self {
        use crate::types::KnownModel;

        Self {
            max_tokens: 1024,
            messages: vec![],
            model: Model::Known(KnownModel::ClaudeSonnet46),
            metadata: None,
            output_format: None,
            stop_sequences: None,
            system: None,
            temperature: None,
            thinking: None,
            tool_choice: None,
            tools: None,
            top_k: None,
            top_p: None,
            stream: false,
            effort: None,
            speed: None,
            output_config: None,

            context_management: None,
            inference_geo: None,
            service_tier: None,
            container: None,
            cache_control: None,
            citations: None,
            skills: None,
            tool_runner: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KnownModel, MessageRole};
    use serde_json::{json, to_value};

    #[test]
    fn message_create_params_non_streaming() {
        let message = MessageParam::new_with_string("Hello, Claude".to_string(), MessageRole::User);

        let params =
            MessageCreateParams::new(1000, vec![message], Model::Known(KnownModel::ClaudeSonnet46))
                .with_system_string("You are a helpful assistant.".to_string())
                .with_temperature(0.125)
                .unwrap();

        let json = to_value(&params).unwrap();
        assert!(!params.stream);

        assert_eq!(
            json,
            json!({
                "max_tokens": 1000,
                "messages": [
                    {
                        "role": "user",
                        "content": "Hello, Claude"
                    }
                ],
                "model": "claude-sonnet-4-6",
                "system": "You are a helpful assistant.",
                "temperature": 0.125,
                "stream": false
            })
        );
    }

    #[test]
    fn message_create_params_streaming() {
        let message = MessageParam::new_with_string("Hello, Claude".to_string(), MessageRole::User);

        let params = MessageCreateParams::new_streaming(
            1000,
            vec![message],
            Model::Known(KnownModel::ClaudeSonnet46),
        );

        let json = to_value(&params).unwrap();
        assert!(params.stream);

        assert_eq!(
            json,
            json!({
                "max_tokens": 1000,
                "messages": [
                    {
                        "role": "user",
                        "content": "Hello, Claude"
                    }
                ],
                "model": "claude-sonnet-4-6",
                "stream": true
            })
        );
    }

    #[test]
    fn message_create_params_with_stream() {
        let message = MessageParam::new_with_string("Hello, Claude".to_string(), MessageRole::User);

        let params =
            MessageCreateParams::new(1000, vec![message], Model::Known(KnownModel::ClaudeSonnet46))
                .with_stream(true);

        let json = to_value(&params).unwrap();
        assert!(params.stream);

        assert_eq!(
            json,
            json!({
                "max_tokens": 1000,
                "messages": [
                    {
                        "role": "user",
                        "content": "Hello, Claude"
                    }
                ],
                "model": "claude-sonnet-4-6",
                "stream": true
            })
        );
    }

    #[test]
    fn message_create_params_simple() {
        let params = MessageCreateParams::simple("Hello, world!", KnownModel::ClaudeSonnet46);

        assert_eq!(params.max_tokens, 1024);
        assert_eq!(params.messages.len(), 1);
        assert_eq!(params.messages[0].role, MessageRole::User);
        assert!(!params.stream);
    }

    #[test]
    fn message_create_params_simple_streaming() {
        let params =
            MessageCreateParams::simple_streaming("Tell me a joke", KnownModel::ClaudeSonnet46);

        assert_eq!(params.max_tokens, 1024);
        assert_eq!(params.messages.len(), 1);
        assert!(params.stream);
    }

    #[test]
    fn message_create_params_default() {
        let params = MessageCreateParams::default();

        assert_eq!(params.max_tokens, 1024);
        assert_eq!(params.messages.len(), 0);
        assert!(!params.stream);
    }

    #[test]
    fn message_create_params_with_message() {
        let params = MessageCreateParams::default()
            .with_message("Hello")
            .with_message(MessageParam::assistant("Hi there"));

        assert_eq!(params.messages.len(), 2);
        assert_eq!(params.messages[0].role, MessageRole::User);
        assert_eq!(params.messages[1].role, MessageRole::Assistant);
    }

    #[test]
    fn message_create_params_ergonomic_system() {
        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46)
            .with_system("You are a helpful assistant.");

        assert!(params.system.is_some());
    }

    #[test]
    fn requires_structured_outputs_beta_with_output_format() {
        use crate::types::OutputFormat;

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46)
            .with_output_format(OutputFormat::json_schema(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"],
                "additionalProperties": false
            })));

        assert!(
            params.requires_structured_outputs_beta(),
            "params with output_format should require structured outputs beta"
        );
    }

    #[test]
    fn requires_structured_outputs_beta_with_strict_tool() {
        use crate::types::{ToolParam, ToolUnionParam};

        let tool = ToolParam::new(
            "get_weather".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"],
                "additionalProperties": false
            }),
        )
        .with_strict(true);

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46)
            .with_tools(vec![ToolUnionParam::CustomTool(tool)]);

        assert!(
            params.requires_structured_outputs_beta(),
            "params with strict tool should require structured outputs beta"
        );
    }

    #[test]
    fn requires_structured_outputs_beta_with_non_strict_tool() {
        use crate::types::{ToolParam, ToolUnionParam};

        let tool = ToolParam::new(
            "get_weather".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"]
            }),
        );

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46)
            .with_tools(vec![ToolUnionParam::CustomTool(tool)]);

        assert!(
            !params.requires_structured_outputs_beta(),
            "params with non-strict tool should not require structured outputs beta"
        );
    }

    #[test]
    fn requires_structured_outputs_beta_without_features() {
        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);

        assert!(
            !params.requires_structured_outputs_beta(),
            "params without output_format or strict tools should not require structured outputs beta"
        );
    }
}
