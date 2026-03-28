use serde::{Deserialize, Serialize};

use crate::types::{
    CompactionMetadata, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    MessageDeltaEvent, MessageStartEvent, MessageStopEvent,
};

/// An API error object for stream error events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Error type string.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error message.
    pub message: String,
}

/// An event in a message stream.
///
/// This enum represents all possible events that can occur when streaming
/// messages from the Anthropic API. Events are delivered in a specific order:
/// message_start, then potentially multiple content_block events, and finally
/// message_stop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum MessageStreamEvent {
    /// A periodic ping event to keep the connection alive.
    #[serde(rename = "ping")]
    Ping,

    /// Indicates the start of a new message in the stream.
    #[serde(rename = "message_start")]
    MessageStart(MessageStartEvent),

    /// Provides incremental updates to the message being generated.
    #[serde(rename = "message_delta")]
    MessageDelta(MessageDeltaEvent),

    /// Marks the beginning of a new content block within the message.
    #[serde(rename = "content_block_start")]
    ContentBlockStart(ContentBlockStartEvent),

    /// Provides incremental updates to the current content block.
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta(ContentBlockDeltaEvent),

    /// Indicates that the current content block is complete.
    #[serde(rename = "content_block_stop")]
    ContentBlockStop(ContentBlockStopEvent),

    /// Marks the end of the message stream.
    #[serde(rename = "message_stop")]
    MessageStop(MessageStopEvent),

    /// Fine-grained tool streaming: parameter start (Req 20).
    #[serde(rename = "tool_input_start")]
    ToolInputStart {
        /// The tool use ID this parameter belongs to.
        tool_use_id: String,
        /// The name of the parameter being streamed.
        parameter_name: String,
    },

    /// Fine-grained tool streaming: parameter delta (Req 20).
    #[serde(rename = "tool_input_delta")]
    ToolInputDelta {
        /// The tool use ID this parameter belongs to.
        tool_use_id: String,
        /// The name of the parameter being streamed.
        parameter_name: String,
        /// A fragment of the parameter value.
        value_fragment: String,
    },

    /// Compaction event during streaming (Req 3.8, 16.2).
    #[serde(rename = "compaction")]
    CompactionEvent(CompactionMetadata),

    /// Stream error (Req 3.9).
    #[serde(rename = "stream_error")]
    StreamError {
        /// The error details.
        error: ApiError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    #[test]
    fn message_stream_event_deserialization_message_start() {
        let json = json!({
            "type": "message_start",
            "message": {
                "id": "msg_012345",
                "content": [],
                "model": "claude-3-sonnet-20240229",
                "role": "assistant",
                "type": "message",
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 100
                }
            }
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::MessageStart(_) => {}
            _ => panic!("Expected MessageStart variant"),
        }
    }

    #[test]
    fn message_stream_event_deserialization_message_delta() {
        let json = json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn"
            },
            "usage": {
                "input_tokens": 50,
                "output_tokens": 100
            }
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::MessageDelta(_) => {}
            _ => panic!("Expected MessageDelta variant"),
        }
    }

    #[test]
    fn message_stream_event_deserialization_message_stop() {
        let json = json!({
            "type": "message_stop"
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::MessageStop(_) => {}
            _ => panic!("Expected MessageStop variant"),
        }
    }

    #[test]
    fn message_stream_event_deserialization_content_block_start() {
        let json = json!({
            "type": "content_block_start",
            "content_block": {
                "text": "Hello, I'm Claude.",
                "type": "text"
            },
            "index": 0
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockStart(_) => {}
            _ => panic!("Expected ContentBlockStart variant"),
        }
    }

    #[test]
    fn message_stream_event_deserialization_content_block_delta() {
        let json = json!({
            "type": "content_block_delta",
            "delta": {
                "text": "Hello, I'm Claude.",
                "type": "text_delta"
            },
            "index": 0
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockDelta(_) => {}
            _ => panic!("Expected ContentBlockDelta variant"),
        }
    }

    #[test]
    fn message_stream_event_deserialization_content_block_stop() {
        let json = json!({
            "type": "content_block_stop",
            "index": 0
        });

        let event: MessageStreamEvent = from_value(json).unwrap();
        match event {
            MessageStreamEvent::ContentBlockStop(_) => {}
            _ => panic!("Expected ContentBlockStop variant"),
        }
    }
}
