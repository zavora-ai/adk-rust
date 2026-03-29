use serde::{Deserialize, Serialize};

use crate::types::Message;

/// An event that represents the start of a message in a streaming response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageStartEvent {
    /// The message that is starting.
    pub message: Message,
}

impl MessageStartEvent {
    /// Create a new `MessageStartEvent` with the given message.
    pub fn new(message: Message) -> Self {
        Self { message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, MessageRole, Model, TextBlock, Usage};
    use serde_json::{json, to_value};

    #[test]
    fn message_start_event_serialization() {
        let text_block = TextBlock::new("Hello, I'm Claude.".to_string());
        let content = vec![ContentBlock::Text(text_block)];
        let model = Model::Known(crate::types::KnownModel::ClaudeSonnet46);
        let usage = Usage::new(50, 100);

        let message = Message::new("msg_012345".to_string(), content, model, usage);

        let event = MessageStartEvent::new(message);

        let json = to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({
                "message": {
                    "id": "msg_012345",
                    "content": [
                        {
                            "text": "Hello, I'm Claude.",
                            "type": "text"
                        }
                    ],
                    "model": "claude-sonnet-4-6",
                    "role": "assistant",
                    "type": "message",
                    "usage": {
                        "input_tokens": 50,
                        "output_tokens": 100
                    }
                }
            })
        );
    }

    #[test]
    fn message_start_event_deserialization() {
        let json = json!({
            "message": {
                "id": "msg_012345",
                "content": [
                    {
                        "text": "Hello, I'm Claude.",
                        "type": "text"
                    }
                ],
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "type": "message",
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 100
                }
            }
        });

        let event: MessageStartEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.message.id, "msg_012345");
        assert_eq!(event.message.role, MessageRole::Assistant);
    }
}
