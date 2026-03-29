use serde::{Deserialize, Serialize};

use crate::types::{
    MessageCreateParams, MessageParam, Metadata, Model, SystemPrompt, ThinkingConfig, ToolChoice,
    ToolUnionParam,
};

/// A template for creating message parameters.
///
/// Every field in this template is optional, allowing you to specify only the
/// fields you want to override. Use the `apply` method to apply the template
/// to a `MessageCreateParams` instance.
///
/// # Example
///
/// ```
/// # use adk_anthropic::{MessageCreateTemplate, MessageCreateParams, KnownModel};
/// let template = MessageCreateTemplate::new()
///     .with_max_tokens(2048)
///     .with_temperature(0.7)
///     .unwrap();
///
/// let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);
/// let params = template.apply(params);
///
/// assert_eq!(params.max_tokens, 2048);
/// assert_eq!(params.temperature, Some(0.7));
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessageCreateTemplate {
    /// The maximum number of tokens to generate before stopping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Input messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<MessageParam>>,

    /// The model that will complete your prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,

    /// An object describing metadata about the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,

    /// Custom text sequences that will cause the model to stop generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    /// System prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,

    /// Amount of randomness injected into the response.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl MessageCreateTemplate {
    /// Helper function to validate a float value is within the 0.0-1.0 range.
    #[inline]
    fn validate_float_range(value: f32, field_name: &str) -> Result<(), crate::Error> {
        if (0.0..=1.0).contains(&value) && value.is_finite() {
            return Ok(());
        }

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

    /// Create a new empty template.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the max_tokens field.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set the messages field.
    pub fn with_messages(mut self, messages: Vec<MessageParam>) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Set the model field.
    pub fn with_model(mut self, model: impl Into<Model>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the metadata field.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set the stop_sequences field.
    pub fn with_stop_sequences(mut self, stop_sequences: Vec<String>) -> Self {
        self.stop_sequences = Some(stop_sequences);
        self
    }

    /// Set the system prompt field.
    pub fn with_system(mut self, system: impl Into<SystemPrompt>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Set the temperature field.
    pub fn with_temperature(mut self, temperature: f32) -> Result<Self, crate::Error> {
        Self::validate_float_range(temperature, "temperature")?;
        self.temperature = Some(temperature);
        Ok(self)
    }

    /// Set the thinking configuration field.
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Set the tool_choice field.
    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    /// Set the tools field.
    pub fn with_tools(mut self, tools: Vec<ToolUnionParam>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the top_k field.
    pub fn with_top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Set the top_p field.
    pub fn with_top_p(mut self, top_p: f32) -> Result<Self, crate::Error> {
        Self::validate_float_range(top_p, "top_p")?;
        self.top_p = Some(top_p);
        Ok(self)
    }

    /// Set the stream field.
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }

    /// Merge another template into this one, overriding any fields set in `other`.
    pub fn merge(mut self, other: MessageCreateTemplate) -> Self {
        if other.max_tokens.is_some() {
            self.max_tokens = other.max_tokens;
        }
        if other.messages.is_some() {
            self.messages = other.messages;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
        if other.metadata.is_some() {
            self.metadata = other.metadata;
        }
        if other.stop_sequences.is_some() {
            self.stop_sequences = other.stop_sequences;
        }
        if other.system.is_some() {
            self.system = other.system;
        }
        if other.temperature.is_some() {
            self.temperature = other.temperature;
        }
        if other.thinking.is_some() {
            self.thinking = other.thinking;
        }
        if other.tool_choice.is_some() {
            self.tool_choice = other.tool_choice;
        }
        if other.tools.is_some() {
            self.tools = other.tools;
        }
        if other.top_k.is_some() {
            self.top_k = other.top_k;
        }
        if other.top_p.is_some() {
            self.top_p = other.top_p;
        }
        if other.stream.is_some() {
            self.stream = other.stream;
        }
        self
    }

    /// Apply this template to the given `MessageCreateParams`.
    ///
    /// Fields that are `Some` in the template will override the corresponding
    /// fields in the params. Fields that are `None` in the template will leave
    /// the params unchanged.
    pub fn apply(self, mut params: MessageCreateParams) -> MessageCreateParams {
        if let Some(max_tokens) = self.max_tokens {
            params.max_tokens = max_tokens;
        }
        if let Some(messages) = self.messages {
            params.messages = messages;
        }
        if let Some(model) = self.model {
            params.model = model;
        }
        if let Some(metadata) = self.metadata {
            params.metadata = Some(metadata);
        }
        if let Some(stop_sequences) = self.stop_sequences {
            params.stop_sequences = Some(stop_sequences);
        }
        if let Some(system) = self.system {
            params.system = Some(system);
        }
        if let Some(temperature) = self.temperature {
            params.temperature = Some(temperature);
        }
        if let Some(thinking) = self.thinking {
            params.thinking = Some(thinking);
        }
        if let Some(tool_choice) = self.tool_choice {
            params.tool_choice = Some(tool_choice);
        }
        if let Some(tools) = self.tools {
            params.tools = Some(tools);
        }
        if let Some(top_k) = self.top_k {
            params.top_k = Some(top_k);
        }
        if let Some(top_p) = self.top_p {
            params.top_p = Some(top_p);
        }
        if let Some(stream) = self.stream {
            params.stream = stream;
        }
        params
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KnownModel;

    #[test]
    fn template_new_creates_empty_template() {
        let template = MessageCreateTemplate::new();

        assert!(template.max_tokens.is_none());
        assert!(template.messages.is_none());
        assert!(template.model.is_none());
        assert!(template.metadata.is_none());
        assert!(template.stop_sequences.is_none());
        assert!(template.system.is_none());
        assert!(template.temperature.is_none());
        assert!(template.thinking.is_none());
        assert!(template.tool_choice.is_none());
        assert!(template.tools.is_none());
        assert!(template.top_k.is_none());
        assert!(template.top_p.is_none());
        assert!(template.stream.is_none());
    }

    #[test]
    fn template_with_max_tokens() {
        let template = MessageCreateTemplate::new().with_max_tokens(2048);

        assert_eq!(template.max_tokens, Some(2048));
    }

    #[test]
    fn template_with_model() {
        let template = MessageCreateTemplate::new().with_model(KnownModel::ClaudeSonnet46);

        assert!(template.model.is_some());
    }

    #[test]
    fn template_with_temperature_valid() {
        let template = MessageCreateTemplate::new().with_temperature(0.7).unwrap();

        assert_eq!(template.temperature, Some(0.7));
    }

    #[test]
    fn template_with_temperature_invalid() {
        let result = MessageCreateTemplate::new().with_temperature(1.5);

        assert!(result.is_err());
    }

    #[test]
    fn template_with_top_p_valid() {
        let template = MessageCreateTemplate::new().with_top_p(0.9).unwrap();

        assert_eq!(template.top_p, Some(0.9));
    }

    #[test]
    fn template_with_top_p_invalid() {
        let result = MessageCreateTemplate::new().with_top_p(-0.1);

        assert!(result.is_err());
    }

    #[test]
    fn template_with_top_k() {
        let template = MessageCreateTemplate::new().with_top_k(50);

        assert_eq!(template.top_k, Some(50));
    }

    #[test]
    fn template_with_stream() {
        let template = MessageCreateTemplate::new().with_stream(true);

        assert_eq!(template.stream, Some(true));
    }

    #[test]
    fn template_with_system() {
        let template = MessageCreateTemplate::new().with_system("You are a helpful assistant.");

        assert!(template.system.is_some());
    }

    #[test]
    fn template_with_stop_sequences() {
        let template = MessageCreateTemplate::new().with_stop_sequences(vec!["STOP".to_string()]);

        assert_eq!(template.stop_sequences, Some(vec!["STOP".to_string()]));
    }

    #[test]
    fn template_apply_overrides_fields() {
        let template = MessageCreateTemplate::new()
            .with_max_tokens(2048)
            .with_temperature(0.7)
            .unwrap()
            .with_stream(true);

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);
        let params = template.apply(params);

        assert_eq!(params.max_tokens, 2048);
        assert_eq!(params.temperature, Some(0.7));
        assert!(params.stream);
    }

    #[test]
    fn template_apply_preserves_unset_fields() {
        let template = MessageCreateTemplate::new().with_max_tokens(2048);

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46)
            .with_system("System prompt");
        let params = template.apply(params);

        assert_eq!(params.max_tokens, 2048);
        assert!(params.system.is_some());
    }

    #[test]
    fn template_merge_overrides_fields() {
        let base = MessageCreateTemplate::new()
            .with_max_tokens(1024)
            .with_model(KnownModel::ClaudeSonnet46)
            .with_stream(false);
        let override_template =
            MessageCreateTemplate::new().with_max_tokens(2048).with_stream(true);

        let merged = base.merge(override_template);

        assert_eq!(merged.max_tokens, Some(2048));
        assert!(merged.model.is_some());
        assert_eq!(merged.stream, Some(true));
    }

    #[test]
    fn template_apply_overrides_model() {
        let template = MessageCreateTemplate::new().with_model(KnownModel::ClaudeHaiku45);

        let params = MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46);
        let original_model = params.model.clone();
        let params = template.apply(params);

        assert_ne!(params.model, original_model);
        assert_eq!(params.model, Model::Known(KnownModel::ClaudeHaiku45));
    }

    #[test]
    fn template_chained_builders() {
        let template = MessageCreateTemplate::new()
            .with_max_tokens(4096)
            .with_model(KnownModel::ClaudeSonnet46)
            .with_system("Be concise.")
            .with_top_k(40)
            .with_stream(false);

        assert_eq!(template.max_tokens, Some(4096));
        assert!(template.model.is_some());
        assert!(template.system.is_some());
        assert_eq!(template.top_k, Some(40));
        assert_eq!(template.stream, Some(false));
    }

    #[test]
    fn template_serialization() {
        let template =
            MessageCreateTemplate::new().with_max_tokens(1024).with_temperature(0.5).unwrap();

        let json = serde_json::to_value(&template).unwrap();

        assert_eq!(json["max_tokens"], 1024);
        assert_eq!(json["temperature"], 0.5);
        assert!(json.get("model").is_none());
        assert!(json.get("stream").is_none());
    }

    #[test]
    fn template_deserialization() {
        let json = r#"{"max_tokens": 2048, "temperature": 0.8}"#;
        let template: MessageCreateTemplate = serde_json::from_str(json).unwrap();

        assert_eq!(template.max_tokens, Some(2048));
        assert_eq!(template.temperature, Some(0.8));
        assert!(template.model.is_none());
    }
}
