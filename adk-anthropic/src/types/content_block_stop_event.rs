use serde::{Deserialize, Serialize};

/// An event that represents the end of a content block in a streaming response.
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockStopEvent {
    /// The index of the content block that is ending.
    pub index: usize,
}

impl ContentBlockStopEvent {
    /// Create a new `ContentBlockStopEvent` with the given index.
    pub fn new(index: usize) -> Self {
        Self { index }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn content_block_stop_event_serialization() {
        let event = ContentBlockStopEvent::new(0);

        let json = to_value(event).unwrap();
        assert_eq!(
            json,
            json!({
                "index": 0
            })
        );
    }

    #[test]
    fn content_block_stop_event_deserialization() {
        let json = json!({
            "index": 0
        });

        let event: ContentBlockStopEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.index, 0);
    }
}
