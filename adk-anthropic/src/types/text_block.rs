use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::types::{CacheControlEphemeral, TextCitation};

/// A block of text content in a message.
///
/// TextBlocks contain plain text content and optional citations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextBlock {
    /// Optional citations supporting the text block.
    ///
    /// The type of citation returned will depend on the type of document being cited.
    /// Citing a PDF results in `page_location`, plain text results in `char_location`,
    /// and content document results in `content_block_location`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<TextCitation>>,

    /// The text content.
    pub text: String,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl TextBlock {
    /// Creates a new TextBlock with the specified text.
    pub fn new<S: Into<String>>(text: S) -> Self {
        Self { text: text.into(), citations: None, cache_control: None }
    }

    /// Creates a new TextBlock with the specified text and citations.
    pub fn with_citations<S: Into<String>>(text: S, citations: Vec<TextCitation>) -> Self {
        Self { text: text.into(), citations: Some(citations), cache_control: None }
    }

    /// Add a cache control to this text block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Add a single citation to this text block.
    pub fn with_citation(mut self, citation: TextCitation) -> Self {
        if let Some(citations) = &mut self.citations {
            citations.push(citation);
        } else {
            self.citations = Some(vec![citation]);
        }
        self
    }

    /// Returns the number of citations if any, or 0 if there are none.
    pub fn citation_count(&self) -> usize {
        self.citations.as_ref().map_or(0, |c| c.len())
    }

    /// Returns true if this text block has citations.
    pub fn has_citations(&self) -> bool {
        self.citation_count() > 0
    }
}

impl FromStr for TextBlock {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CitationCharLocation;
    use serde_json::{json, to_value};

    #[test]
    fn text_block_serialization() {
        let text_block = TextBlock::new("This is some text content.");

        let json = serde_json::to_value(&text_block).unwrap();
        let expected = serde_json::json!({"text": "This is some text content."});

        assert_eq!(json, expected);
    }

    #[test]
    fn text_block_with_citations_serialization() {
        let char_location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let citation = TextCitation::CharLocation(char_location);

        let text_block =
            TextBlock::with_citations("This is some text content with a citation.", vec![citation]);

        // For this test, we'll directly check the structure instead of the exact string
        // since the citation order might change and cause test flakiness
        let json_value = serde_json::to_value(&text_block).unwrap();

        // Check that basic structure is correct
        assert!(json_value.is_object());
        let obj = json_value.as_object().unwrap();

        // Check text field
        assert_eq!(
            obj.get("text").unwrap().as_str().unwrap(),
            "This is some text content with a citation."
        );

        // Check citations array exists and has one element
        assert!(obj.get("citations").unwrap().is_array());
        let citations = obj.get("citations").unwrap().as_array().unwrap();
        assert_eq!(citations.len(), 1);

        // Check citation content
        let citation = &citations[0];
        assert_eq!(citation.get("cited_text").unwrap().as_str().unwrap(), "example text");
        assert_eq!(citation.get("document_index").unwrap().as_i64().unwrap(), 0);
        assert_eq!(citation.get("document_title").unwrap().as_str().unwrap(), "Document Title");
        assert_eq!(citation.get("end_char_index").unwrap().as_i64().unwrap(), 12);
        assert_eq!(citation.get("start_char_index").unwrap().as_i64().unwrap(), 0);
        assert_eq!(citation.get("type").unwrap().as_str().unwrap(), "char_location");
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({
            "text": "This is some text content.",
            "type": "text"
        });
        let text_block: TextBlock = serde_json::from_value(json).unwrap();

        assert_eq!(text_block.text, "This is some text content.");
        assert!(text_block.citations.is_none());
    }

    #[test]
    fn helper_methods() {
        let text_block = TextBlock::new("Simple text");
        assert_eq!(text_block.citation_count(), 0);
        assert!(!text_block.has_citations());

        let char_location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let citation = TextCitation::CharLocation(char_location);

        let text_block = TextBlock::with_citations("Text with citation", vec![citation]);

        assert_eq!(text_block.citation_count(), 1);
        assert!(text_block.has_citations());
    }

    #[test]
    fn text_block_with_cache_control() {
        let cache_control = CacheControlEphemeral::new();
        let text_block = TextBlock::new("Sample text content").with_cache_control(cache_control);

        let json = to_value(&text_block).unwrap();

        assert_eq!(
            json,
            json!({
                "text": "Sample text content",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn text_block_with_citation() {
        let citation = TextCitation::CharLocation(CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        });

        let text_block = TextBlock::new("Sample text content").with_citation(citation);

        let json = to_value(&text_block).unwrap();

        assert_eq!(
            json,
            json!({
                "text": "Sample text content",
                "citations": [
                    {
                        "cited_text": "example text",
                        "document_index": 0,
                        "document_title": "Document Title",
                        "end_char_index": 12,
                        "start_char_index": 0,
                        "type": "char_location"
                    }
                ]
            })
        );
    }

    #[test]
    fn from_str() {
        let text_block = "Sample text content".parse::<TextBlock>().unwrap();
        assert_eq!(text_block.text, "Sample text content");
    }
}
