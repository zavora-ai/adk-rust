//! Event types for realtime communication.
//!
//! These events follow a unified model inspired by the OpenAI Agents SDK,
//! abstracting over provider-specific event formats.
//!
//! Audio data is transported as raw bytes (`Vec<u8>`) internally but serialized
//! as base64 on the wire for JSON compatibility.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Custom serde for base64-encoded audio ───────────────────────────────

fn deserialize_audio_bytes<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    base64::engine::general_purpose::STANDARD.decode(&s).map_err(serde::de::Error::custom)
}

fn serialize_audio_bytes<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = base64::engine::general_purpose::STANDARD.encode(bytes);
    serializer.serialize_str(&s)
}

// ── Client Events ───────────────────────────────────────────────────────

/// Events sent from the client to the realtime server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    /// Update session configuration.
    #[serde(rename = "session.update")]
    SessionUpdate {
        /// Updated session configuration.
        session: Value,
    },

    /// Append audio to the input buffer.
    #[serde(rename = "input_audio_buffer.append")]
    AudioDelta {
        /// Optional event ID.
        #[serde(skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        /// Audio data (raw bytes, serialized as base64 on the wire).
        #[serde(
            serialize_with = "serialize_audio_bytes",
            deserialize_with = "deserialize_audio_bytes"
        )]
        audio: Vec<u8>,
    },

    /// Commit the current audio buffer (manual mode).
    #[serde(rename = "input_audio_buffer.commit")]
    InputAudioBufferCommit,

    /// Clear the audio input buffer.
    #[serde(rename = "input_audio_buffer.clear")]
    InputAudioBufferClear,

    /// Send a text message or tool response.
    #[serde(rename = "conversation.item.create")]
    ConversationItemCreate {
        /// The conversation item (flexible JSON for provider compatibility).
        item: Value,
    },

    /// Trigger a response from the model.
    #[serde(rename = "response.create")]
    ResponseCreate {
        /// Optional response configuration.
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
    },

    /// Cancel/interrupt the current response.
    #[serde(rename = "response.cancel")]
    ResponseCancel,
}

/// A conversation item for text or tool responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationItem {
    /// Unique ID for this item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Item type: "message" or "function_call_output".
    #[serde(rename = "type")]
    pub item_type: String,
    /// Role: "user", "assistant", or "system".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Content parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentPart>>,
    /// For tool responses: the call ID being responded to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// For tool responses: the output value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// A content part within a conversation item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    /// Content type: "input_text", "input_audio", "text", "audio".
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64-encoded audio content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// Transcript of audio content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

impl ConversationItem {
    /// Create a user text message item.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            id: None,
            item_type: "message".to_string(),
            role: Some("user".to_string()),
            content: Some(vec![ContentPart {
                content_type: "input_text".to_string(),
                text: Some(text.into()),
                audio: None,
                transcript: None,
            }]),
            call_id: None,
            output: None,
        }
    }

    /// Create a tool response item.
    pub fn tool_response(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            id: None,
            item_type: "function_call_output".to_string(),
            role: None,
            content: None,
            call_id: Some(call_id.into()),
            output: Some(output.into()),
        }
    }
}

// ── Server Events ───────────────────────────────────────────────────────

/// Events received from the realtime server.
///
/// This is a unified event type that abstracts over provider-specific formats.
/// Audio data is stored as raw bytes (`Vec<u8>`) — decoded from base64 at the
/// transport boundary so consumers never need to deal with encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Session was created/connected.
    #[serde(rename = "session.created")]
    SessionCreated {
        /// Unique event ID.
        event_id: String,
        /// Session details.
        session: Value,
    },

    /// Session configuration was updated.
    #[serde(rename = "session.updated")]
    SessionUpdated {
        /// Unique event ID.
        event_id: String,
        /// Updated session details.
        session: Value,
    },

    /// Error occurred.
    #[serde(rename = "error")]
    Error {
        /// Unique event ID.
        event_id: String,
        /// Error details.
        error: ErrorInfo,
    },

    /// User speech started (VAD detected).
    #[serde(rename = "input_audio_buffer.speech_started")]
    SpeechStarted {
        /// Unique event ID.
        event_id: String,
        /// Audio start time in milliseconds.
        audio_start_ms: u64,
    },

    /// User speech ended (VAD detected).
    #[serde(rename = "input_audio_buffer.speech_stopped")]
    SpeechStopped {
        /// Unique event ID.
        event_id: String,
        /// Audio end time in milliseconds.
        audio_end_ms: u64,
    },

    /// Audio input buffer was committed.
    #[serde(rename = "input_audio_buffer.committed")]
    AudioCommitted {
        /// Unique event ID.
        event_id: String,
        /// ID of the created item.
        item_id: String,
    },

    /// Audio input buffer was cleared.
    #[serde(rename = "input_audio_buffer.cleared")]
    AudioCleared {
        /// Unique event ID.
        event_id: String,
    },

    /// Conversation item was created.
    #[serde(rename = "conversation.item.created")]
    ItemCreated {
        /// Unique event ID.
        event_id: String,
        /// The created item.
        item: Value,
    },

    /// Response generation started.
    #[serde(rename = "response.created")]
    ResponseCreated {
        /// Unique event ID.
        event_id: String,
        /// Response details.
        response: Value,
    },

    /// Response generation completed.
    #[serde(rename = "response.done")]
    ResponseDone {
        /// Unique event ID.
        event_id: String,
        /// Final response details.
        response: Value,
    },

    /// Response output item added.
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Output index.
        output_index: u32,
        /// The output item.
        item: Value,
    },

    /// Response output item completed.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Output index.
        output_index: u32,
        /// The completed item.
        item: Value,
    },

    /// Audio delta (chunk of output audio as raw bytes).
    #[serde(rename = "response.audio.delta")]
    AudioDelta {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
        /// Audio data (raw bytes, serialized as base64 on the wire).
        #[serde(
            serialize_with = "serialize_audio_bytes",
            deserialize_with = "deserialize_audio_bytes"
        )]
        delta: Vec<u8>,
    },

    /// Audio output completed.
    #[serde(rename = "response.audio.done")]
    AudioDone {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
    },

    /// Text delta (chunk of output text).
    #[serde(rename = "response.text.delta")]
    TextDelta {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
        /// Text content.
        delta: String,
    },

    /// Text output completed.
    #[serde(rename = "response.text.done")]
    TextDone {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
        /// Complete text.
        text: String,
    },

    /// Audio transcript delta.
    #[serde(rename = "response.audio_transcript.delta")]
    TranscriptDelta {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
        /// Transcript delta.
        delta: String,
    },

    /// Audio transcript completed.
    #[serde(rename = "response.audio_transcript.done")]
    TranscriptDone {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Content index.
        content_index: u32,
        /// Complete transcript.
        transcript: String,
    },

    /// Function call arguments delta.
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallDelta {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Call ID.
        call_id: String,
        /// Arguments delta.
        delta: String,
    },

    /// Function call completed.
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallDone {
        /// Unique event ID.
        event_id: String,
        /// Response ID.
        response_id: String,
        /// Item ID.
        item_id: String,
        /// Output index.
        output_index: u32,
        /// Call ID.
        call_id: String,
        /// Function name.
        name: String,
        /// Complete arguments.
        arguments: String,
    },

    /// Rate limit information.
    #[serde(rename = "rate_limits.updated")]
    RateLimitsUpdated {
        /// Unique event ID.
        event_id: String,
        /// Rate limit details.
        rate_limits: Vec<RateLimit>,
    },

    /// Unknown event type (for forward compatibility).
    #[serde(other)]
    Unknown,
}

/// Error information from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// Error type/code.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Error code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human-readable error message.
    pub message: String,
    /// Additional error parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

/// Rate limit information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    /// Limit name.
    pub name: String,
    /// Maximum allowed.
    pub limit: u64,
    /// Currently remaining.
    pub remaining: u64,
    /// Time until reset.
    pub reset_seconds: f64,
}

/// A simplified tool call representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique call ID (used for responses).
    pub call_id: String,
    /// Tool/function name.
    pub name: String,
    /// Arguments as JSON.
    pub arguments: Value,
}

/// A tool response to send back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    /// The call ID being responded to.
    pub call_id: String,
    /// The result/output of the tool execution.
    pub output: Value,
}

impl ToolResponse {
    /// Create a new tool response.
    pub fn new(call_id: impl Into<String>, output: impl Serialize) -> Self {
        Self {
            call_id: call_id.into(),
            output: serde_json::to_value(output).unwrap_or(Value::Null),
        }
    }

    /// Create a tool response from a string output.
    pub fn from_string(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self { call_id: call_id.into(), output: Value::String(output.into()) }
    }
}
