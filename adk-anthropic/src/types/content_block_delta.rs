use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::unknown::tagged_with_unknown;
use crate::types::{CitationsDelta, InputJsonDelta, SignatureDelta, TextDelta, ThinkingDelta};

/// A raw content block delta, representing a streaming update to a content block.
///
/// This type is used for streaming responses from the API, where content blocks
/// are updated incrementally. A delta whose `type` this client does not know
/// deserializes to [`ContentBlockDelta::Unknown`] instead of failing the stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "type")]
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

    /// A delta type this client does not know, kept as the raw JSON the API
    /// sent, `type` field included. It serializes back to that JSON unchanged.
    #[serde(skip)]
    Unknown(Value),
}

tagged_with_unknown!(
    ContentBlockDelta,
    "content block delta",
    ["text_delta", "input_json_delta", "citations_delta", "thinking_delta", "signature_delta"]
);

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
    fn unknown_delta_type_round_trips_as_raw_json() {
        let raw = json!({"type": "future_delta", "payload": "x"});
        let delta: ContentBlockDelta = from_value(raw.clone()).unwrap();
        assert_eq!(delta, ContentBlockDelta::Unknown(raw.clone()));
        assert_eq!(to_value(&delta).unwrap(), raw);
    }

    #[test]
    fn malformed_known_delta_keeps_its_specific_error() {
        let error = from_value::<ContentBlockDelta>(json!({"type": "text_delta"})).unwrap_err();
        assert!(error.to_string().contains("missing field `text`"), "{error}");
    }

    #[test]
    fn known_types_match_every_typed_variant() {
        let error = ContentBlockDelta::deserialize(json!({"type": "__probe__"})).unwrap_err();
        let mut known: Vec<&str> = ContentBlockDelta::KNOWN_TYPES.to_vec();
        known.sort_unstable();
        assert_eq!(crate::types::unknown::expected_variants(&error.to_string()), known);
    }

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
