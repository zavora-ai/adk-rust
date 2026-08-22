use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::types::{
    ContainerInfo, ContentBlock, MessageCreateParams, MessageRole, Model, OutputConfig, SpeedMode,
    StopReason, ThinkingConfig, Usage,
};
use crate::{Error, Result};

const MAX_FALLBACK_MODELS: usize = 3;

/// One explicitly selected server-side fallback model.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbackModel {
    /// Model identifier used for this fallback attempt.
    pub model: String,
    /// Optional output-token limit for this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Optional thinking configuration for this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Optional output configuration for this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    /// Optional speed mode for this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<SpeedMode>,
}

impl FallbackModel {
    /// Create a fallback entry for a model identifier.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_tokens: None,
            thinking: None,
            output_config: None,
            speed: None,
        }
    }

    /// Override the maximum output tokens for this fallback attempt.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Override thinking configuration for this fallback attempt.
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Override output configuration for this fallback attempt.
    pub fn with_output_config(mut self, output_config: OutputConfig) -> Self {
        self.output_config = Some(output_config);
        self
    }

    /// Override speed mode for this fallback attempt.
    pub fn with_speed(mut self, speed: SpeedMode) -> Self {
        self.speed = Some(speed);
        self
    }
}

/// Server-side fallback routing requested from the Claude API.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ServerFallbacks {
    /// Let Anthropic select the recommended model for the refusal category.
    #[default]
    Default,
    /// Try the supplied models in order.
    Models(Vec<FallbackModel>),
}

impl ServerFallbacks {
    /// Use Anthropic's recommended server-side routing.
    pub fn default_routing() -> Self {
        Self::Default
    }

    /// Use an explicit ordered model list.
    ///
    /// # Errors
    ///
    /// Returns a validation error unless the list contains between one and
    /// three distinct, non-empty model identifiers.
    pub fn models(models: Vec<FallbackModel>) -> Result<Self> {
        let fallbacks = Self::Models(models);
        fallbacks.validate(None)?;
        Ok(fallbacks)
    }

    fn validate(&self, primary_model: Option<&Model>) -> Result<()> {
        let Self::Models(models) = self else {
            return Ok(());
        };
        if models.is_empty() || models.len() > MAX_FALLBACK_MODELS {
            return Err(Error::validation(
                format!(
                    "Fallback model list must contain between 1 and {MAX_FALLBACK_MODELS} entries"
                ),
                Some("fallbacks".to_string()),
            ));
        }

        let primary_model = primary_model.map(ToString::to_string);
        let mut seen = HashSet::with_capacity(models.len());
        for fallback in models {
            if fallback.model.trim().is_empty() {
                return Err(Error::validation(
                    "Fallback model identifier cannot be empty".to_string(),
                    Some("fallbacks.model".to_string()),
                ));
            }
            if primary_model.as_deref() == Some(fallback.model.as_str()) {
                return Err(Error::validation(
                    "Fallback model must differ from the requested model".to_string(),
                    Some("fallbacks.model".to_string()),
                ));
            }
            if !seen.insert(fallback.model.as_str()) {
                return Err(Error::validation(
                    format!("Duplicate fallback model: {}", fallback.model),
                    Some("fallbacks.model".to_string()),
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for ServerFallbacks {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Default => serializer.serialize_str("default"),
            Self::Models(models) => models.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ServerFallbacks {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Default(String),
            Models(Vec<FallbackModel>),
        }

        match Wire::deserialize(deserializer)? {
            Wire::Default(value) if value == "default" => Ok(Self::Default),
            Wire::Default(value) => {
                Err(de::Error::custom(format!("unsupported fallback routing mode: {value}")))
            }
            Wire::Models(models) => Ok(Self::Models(models)),
        }
    }
}

/// A message request with server-side refusal fallback enabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackRequest {
    #[serde(flatten)]
    pub(crate) params: MessageCreateParams,
    pub(crate) fallbacks: ServerFallbacks,
}

impl ServerFallbackRequest {
    /// Create and validate a fallback request.
    ///
    /// # Errors
    ///
    /// Returns a validation error for invalid message parameters or fallback
    /// topology.
    pub fn new(params: MessageCreateParams, fallbacks: ServerFallbacks) -> Result<Self> {
        let request = Self { params, fallbacks };
        request.validate()?;
        Ok(request)
    }

    /// Create a request using Anthropic's recommended fallback routing.
    ///
    /// # Errors
    ///
    /// Returns a validation error for invalid message parameters.
    pub fn default_routing(params: MessageCreateParams) -> Result<Self> {
        Self::new(params, ServerFallbacks::Default)
    }

    /// Create a request using an ordered explicit model list.
    ///
    /// This validates structural invariants locally. Anthropic validates each
    /// entry against the primary model's current `allowed_fallback_models`
    /// capability when the request is sent.
    ///
    /// # Errors
    ///
    /// Returns a validation error for invalid message parameters or model list.
    pub fn explicit(params: MessageCreateParams, models: Vec<FallbackModel>) -> Result<Self> {
        Self::new(params, ServerFallbacks::Models(models))
    }

    /// Return the underlying message parameters.
    pub fn params(&self) -> &MessageCreateParams {
        &self.params
    }

    /// Return the configured fallback routing.
    pub fn fallbacks(&self) -> &ServerFallbacks {
        &self.fallbacks
    }

    /// Validate the message parameters and fallback routing.
    ///
    /// # Errors
    ///
    /// Returns a validation error when either configuration is invalid.
    pub fn validate(&self) -> Result<()> {
        self.params.validate()?;
        self.fallbacks.validate(Some(&self.params.model))
    }
}

/// One endpoint in a server-side fallback handoff marker.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRoute {
    /// Model identifier reported by the API.
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum FallbackBlockType {
    #[serde(rename = "fallback")]
    Fallback,
}

/// A response content block marking a fallback between models.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackContentBlock {
    #[serde(rename = "type")]
    block_type: FallbackBlockType,
    /// Model that declined the request.
    pub from: FallbackRoute,
    /// Model selected for the next attempt.
    pub to: FallbackRoute,
}

/// Content returned by a server-side fallback request.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerFallbackContentBlock {
    /// A handoff marker emitted when the API changes models.
    Fallback(FallbackContentBlock),
    /// Any standard Anthropic message content block.
    Standard(ContentBlock),
    /// A response block not yet represented by this crate.
    ///
    /// Keeping the raw value prevents fallback responses from becoming
    /// unreadable when the API introduces a new content block.
    Unknown(Value),
}

/// Details attached to a classifier refusal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefusalDetails {
    /// Detail discriminator, currently `"refusal"`.
    #[serde(rename = "type")]
    pub detail_type: String,
    /// Policy category reported by the classifier, when categorized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Human-readable explanation, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// Opaque credit token for a manual fallback retry, when granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_credit_token: Option<String>,
    /// Whether a manual retry can claim the declined response's partial output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_has_prefill_claim: Option<bool>,
    /// Additional fields added by newer API revisions.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Usage returned by a server-side fallback request.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackUsage {
    /// Standard token and server-tool usage.
    #[serde(flatten)]
    pub usage: Usage,
    /// Per-attempt usage records returned by the beta API.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterations: Vec<Value>,
}

impl ServerFallbackUsage {
    /// Return whether usage records show that a fallback model ran.
    pub fn fallback_ran(&self) -> bool {
        self.iterations.iter().any(|iteration| {
            iteration.get("type").and_then(Value::as_str) == Some("fallback_message")
        })
    }
}

/// Message returned from a server-side fallback request.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackMessage {
    /// Unique message identifier.
    pub id: String,
    /// Optional code-execution container information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerInfo>,
    /// Standard content and fallback handoff markers.
    pub content: Vec<ServerFallbackContentBlock>,
    /// Model that served the returned message.
    pub model: Model,
    /// Conversational role of the response.
    pub role: MessageRole,
    /// Reason generation stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Classifier refusal details, present when the stop reason is `refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<RefusalDetails>,
    /// Matching custom stop sequence, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    /// Object type reported by the API.
    pub r#type: String,
    /// Aggregate and per-attempt usage.
    pub usage: ServerFallbackUsage,
}

impl ServerFallbackMessage {
    /// Return whether a fallback ran and produced the final non-refusal result.
    pub fn served_by_fallback(&self) -> bool {
        let has_handoff = self
            .content
            .iter()
            .any(|block| matches!(block, ServerFallbackContentBlock::Fallback(_)));
        (has_handoff || self.usage.fallback_ran()) && self.stop_reason != Some(StopReason::Refusal)
    }
}

/// Start of a fallback-aware streaming message.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackMessageStartEvent {
    /// Message metadata and initial content.
    pub message: ServerFallbackMessage,
}

/// Start of a fallback-aware streaming content block.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackContentBlockStartEvent {
    /// Content block that is starting.
    pub content_block: ServerFallbackContentBlock,
    /// Block index within the message.
    pub index: usize,
}

/// Final message metadata in a fallback-aware stream.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackMessageDelta {
    /// Reason generation stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Matching custom stop sequence, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    /// Classifier refusal details, when the last attempt refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<RefusalDetails>,
    /// Additional delta fields added by newer API revisions.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Usage in a fallback-aware final streaming delta.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackDeltaUsage {
    /// Standard cumulative streaming usage.
    #[serde(flatten)]
    pub usage: crate::types::MessageDeltaUsage,
    /// Per-attempt usage records, including `fallback_message` entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterations: Vec<Value>,
}

/// Final message-delta event for a fallback-aware stream.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerFallbackMessageDeltaEvent {
    /// Stop metadata for the completed message.
    pub delta: ServerFallbackMessageDelta,
    /// Aggregate and per-attempt usage.
    pub usage: ServerFallbackDeltaUsage,
}

/// Streaming events returned from a server-side fallback request.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerFallbackStreamEvent {
    /// Periodic keepalive.
    #[serde(rename = "ping")]
    Ping,
    /// Message start.
    #[serde(rename = "message_start")]
    MessageStart(ServerFallbackMessageStartEvent),
    /// Message metadata and usage delta.
    #[serde(rename = "message_delta")]
    MessageDelta(ServerFallbackMessageDeltaEvent),
    /// Content block start, including fallback handoff markers.
    #[serde(rename = "content_block_start")]
    ContentBlockStart(ServerFallbackContentBlockStartEvent),
    /// Standard content delta.
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta(crate::types::ContentBlockDeltaEvent),
    /// Standard content block stop.
    #[serde(rename = "content_block_stop")]
    ContentBlockStop(crate::types::ContentBlockStopEvent),
    /// Message stop.
    #[serde(rename = "message_stop")]
    MessageStop(crate::types::MessageStopEvent),
    /// Fine-grained tool parameter start.
    #[serde(rename = "tool_input_start")]
    ToolInputStart {
        /// Tool-use identifier.
        tool_use_id: String,
        /// Parameter name.
        parameter_name: String,
    },
    /// Fine-grained tool parameter delta.
    #[serde(rename = "tool_input_delta")]
    ToolInputDelta {
        /// Tool-use identifier.
        tool_use_id: String,
        /// Parameter name.
        parameter_name: String,
        /// Incremental parameter value.
        value_fragment: String,
    },
    /// Context compaction event.
    #[serde(rename = "compaction")]
    CompactionEvent(crate::types::CompactionMetadata),
    /// API-reported stream error.
    #[serde(rename = "stream_error")]
    StreamError {
        /// Error details.
        error: crate::types::ApiError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_and_explicit_wire_shapes() {
        assert_eq!(serde_json::to_value(ServerFallbacks::Default).unwrap(), json!("default"));
        let explicit =
            ServerFallbacks::models(vec![FallbackModel::new("claude-opus-4-8")]).unwrap();
        assert_eq!(serde_json::to_value(explicit).unwrap(), json!([{"model": "claude-opus-4-8"}]));
    }

    #[test]
    fn explicit_topology_is_validated() {
        let params =
            MessageCreateParams::simple("hello", Model::Custom("claude-fable-5".to_string()));
        let duplicate =
            vec![FallbackModel::new("claude-opus-4-8"), FallbackModel::new("claude-opus-4-8")];
        assert!(ServerFallbackRequest::explicit(params.clone(), duplicate).is_err());
        assert!(ServerFallbackRequest::explicit(params.clone(), Vec::new()).is_err());
        assert!(
            ServerFallbackRequest::explicit(
                params.clone(),
                vec![
                    FallbackModel::new("model-1"),
                    FallbackModel::new("model-2"),
                    FallbackModel::new("model-3"),
                    FallbackModel::new("model-4"),
                ],
            )
            .is_err()
        );
        assert!(
            ServerFallbackRequest::explicit(params, vec![FallbackModel::new("claude-fable-5")])
                .is_err()
        );
    }

    #[test]
    fn response_and_stream_fallback_blocks_deserialize() {
        let message: ServerFallbackMessage = serde_json::from_value(json!({
            "id": "msg_test",
            "content": [{
                "type": "fallback",
                "from": {"model": "claude-fable-5"},
                "to": {"model": "claude-opus-4-8"}
            }, {"type": "text", "text": "ok"}],
            "model": "claude-opus-4-8",
            "role": "assistant",
            "stop_reason": "end_turn",
            "stop_details": null,
            "stop_sequence": null,
            "type": "message",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "iterations": [{"type": "fallback_message"}]
            }
        }))
        .unwrap();
        assert!(message.served_by_fallback());
        assert!(matches!(message.content[0], ServerFallbackContentBlock::Fallback(_)));

        let event: ServerFallbackStreamEvent = serde_json::from_value(json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "fallback",
                "from": {"model": "claude-fable-5"},
                "to": {"model": "claude-opus-4-8"}
            }
        }))
        .unwrap();
        assert!(matches!(event, ServerFallbackStreamEvent::ContentBlockStart(_)));

        let event: ServerFallbackStreamEvent = serde_json::from_value(json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "iterations": [{"type": "fallback_message", "model": "claude-opus-4-8"}]
            }
        }))
        .unwrap();
        let ServerFallbackStreamEvent::MessageDelta(event) = event else {
            panic!("expected message delta");
        };
        assert_eq!(event.usage.iterations[0]["type"], "fallback_message");
    }
}
