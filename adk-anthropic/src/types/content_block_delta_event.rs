use serde::{Deserialize, Serialize};

use crate::types::ContentBlockDelta;

/// An event that represents a delta update to a content block in a streaming response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockDeltaEvent {
    /// The delta update to the content block.
    pub delta: ContentBlockDelta,

    /// The index of the content block being updated.
    pub index: usize,
}

impl ContentBlockDeltaEvent {
    /// Create a new `ContentBlockDeltaEvent` with the given delta and index.
    pub fn new(delta: ContentBlockDelta, index: usize) -> Self {
        Self { delta, index }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TextDelta;
    use serde_json::{json, to_value};

    #[test]
    fn content_block_delta_event_serialization() {
        let text_delta = TextDelta::new("Hello world".to_string());
        let delta = ContentBlockDelta::TextDelta(text_delta);
        let event = ContentBlockDeltaEvent::new(delta, 0);

        let json = to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({
                "delta": {
                    "text": "Hello world",
                    "type": "text_delta"
                },
                "index": 0
            })
        );
    }

    #[test]
    fn content_block_delta_event_deserialization() {
        let json = json!({
            "delta": {
                "text": "Hello world",
                "type": "text_delta"
            },
            "index": 0
        });

        let event: ContentBlockDeltaEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.index, 0);

        match event.delta {
            ContentBlockDelta::TextDelta(text_delta) => {
                assert_eq!(text_delta.text, "Hello world");
            }
            _ => panic!("Expected TextDelta variant"),
        }
    }
}
