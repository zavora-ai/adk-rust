use serde::{Deserialize, Serialize};

use crate::types::{CitationsDelta, InputJsonDelta, SignatureDelta, TextDelta, ThinkingDelta};

/// A raw content block delta, representing a streaming update to a content block.
///
/// This type is used for streaming responses from the API, where content blocks
/// are updated incrementally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlockDelta {
    /// A text delta.
    #[serde(rename = "text_delta")]
    TextDelta(TextDelta),

    /// An input JSON delta.
    #[serde(rename = "input_json_delta")]
    InputJsonDelta(InputJsonDelta),

    /// A citations delta.
    #[serde(rename = "citations_delta")]
    CitationsDelta(CitationsDelta),

    /// A thinking delta.
    #[serde(rename = "thinking_delta")]
    ThinkingDelta(ThinkingDelta),

    /// A signature delta.
    #[serde(rename = "signature_delta")]
    SignatureDelta(SignatureDelta),
}

impl ContentBlockDelta {
    /// Create a new `ContentBlockDelta` from a text delta.
    pub fn from_text_delta(text_delta: TextDelta) -> Self {
        ContentBlockDelta::TextDelta(text_delta)
    }

    /// Create a new `ContentBlockDelta` from an input JSON delta.
    pub fn from_input_json_delta(input_json_delta: InputJsonDelta) -> Self {
        ContentBlockDelta::InputJsonDelta(input_json_delta)
    }

    /// Create a new `ContentBlockDelta` from a citations delta.
    pub fn from_citations_delta(citations_delta: CitationsDelta) -> Self {
        ContentBlockDelta::CitationsDelta(citations_delta)
    }

    /// Create a new `ContentBlockDelta` from a thinking delta.
    pub fn from_thinking_delta(thinking_delta: ThinkingDelta) -> Self {
        ContentBlockDelta::ThinkingDelta(thinking_delta)
    }

    /// Create a new `ContentBlockDelta` from a signature delta.
    pub fn from_signature_delta(signature_delta: SignatureDelta) -> Self {
        ContentBlockDelta::SignatureDelta(signature_delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json, to_value};

    #[test]
    fn text_delta_serialization() {
        let text_delta = TextDelta::new("Hello world".to_string());
        let delta = ContentBlockDelta::TextDelta(text_delta);

        let json = to_value(&delta).unwrap();
        assert_eq!(
            json,
            json!({
                "text": "Hello world",
                "type": "text_delta"
            })
        );
    }

    #[test]
    fn input_json_delta_serialization() {
        let input_json_delta = InputJsonDelta::new(r#"{"key":"#.to_string());
        let delta = ContentBlockDelta::InputJsonDelta(input_json_delta);

        let json = to_value(&delta).unwrap();
        assert_eq!(
            json,
            json!({
                "partial_json": r#"{"key":"#,
                "type": "input_json_delta"
            })
        );
    }

    #[test]
    fn citations_delta_serialization() {
        let char_location = crate::types::CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let citations_delta = CitationsDelta::with_char_location(char_location);
        let delta = ContentBlockDelta::CitationsDelta(citations_delta);

        let json = to_value(&delta).unwrap();
        assert_eq!(
            json,
            json!({
                "citation": {
                    "type": "char_location",
                    "cited_text": "example text",
                    "document_index": 0,
                    "document_title": "Document Title",
                    "end_char_index": 12,
                    "start_char_index": 0
                },
                "type": "citations_delta"
            })
        );
    }

    #[test]
    fn thinking_delta_serialization() {
        let thinking_delta = ThinkingDelta::new("Let me think about this...".to_string());
        let delta = ContentBlockDelta::ThinkingDelta(thinking_delta);

        let json = to_value(&delta).unwrap();
        assert_eq!(
            json,
            json!({
                "thinking": "Let me think about this...",
                "type": "thinking_delta"
            })
        );
    }

    #[test]
    fn signature_delta_serialization() {
        let signature_delta = SignatureDelta::new("Robert Paulson".to_string());
        let delta = ContentBlockDelta::SignatureDelta(signature_delta);

        let json = to_value(&delta).unwrap();
        assert_eq!(
            json,
            json!({
                "signature": "Robert Paulson",
                "type": "signature_delta"
            })
        );
    }

    #[test]
    fn deserialization() {
        let json = json!({
            "text": "Hello world",
            "type": "text_delta"
        });

        let delta: ContentBlockDelta = from_value(json).unwrap();
        match delta {
            ContentBlockDelta::TextDelta(text_delta) => {
                assert_eq!(text_delta.text, "Hello world");
            }
            _ => panic!("Expected TextDelta variant"),
        }
    }
}
