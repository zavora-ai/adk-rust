use serde::{Deserialize, Serialize};

use crate::types::{
    CitationCharLocation, CitationContentBlockLocation, CitationPageLocation,
    CitationWebSearchResultLocation,
};

/// Represents a citation object that could be any of the supported citation types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Citation {
    /// Citation referencing a character location in source text.
    #[serde(rename = "char_location")]
    CharLocation(CitationCharLocation),

    /// Citation referencing a page location in a document.
    #[serde(rename = "page_location")]
    PageLocation(CitationPageLocation),

    /// Citation referencing a content block location.
    #[serde(rename = "content_block_location")]
    ContentBlockLocation(CitationContentBlockLocation),

    /// Citation referencing a web search result location.
    #[serde(rename = "web_search_result_location")]
    WebSearchResultLocation(CitationWebSearchResultLocation),
}

/// A delta representing a new citation in a streaming response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationsDelta {
    /// The citation that was added
    pub citation: Citation,
}

impl CitationsDelta {
    /// Creates a new CitationsDelta with a CitationCharLocation
    pub fn with_char_location(location: CitationCharLocation) -> Self {
        Self { citation: Citation::CharLocation(location) }
    }

    /// Creates a new CitationsDelta with a CitationPageLocation
    pub fn with_page_location(location: CitationPageLocation) -> Self {
        Self { citation: Citation::PageLocation(location) }
    }

    /// Creates a new CitationsDelta with a CitationContentBlockLocation
    pub fn with_content_block_location(location: CitationContentBlockLocation) -> Self {
        Self { citation: Citation::ContentBlockLocation(location) }
    }

    /// Creates a new CitationsDelta with a CitationWebSearchResultLocation
    pub fn with_web_search_result_location(location: CitationWebSearchResultLocation) -> Self {
        Self { citation: Citation::WebSearchResultLocation(location) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_location_serialization() {
        let char_location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let delta = CitationsDelta::with_char_location(char_location);

        let json = serde_json::to_string(&delta).unwrap();
        let expected = r#"{"citation":{"type":"char_location","cited_text":"example text","document_index":0,"document_title":"Document Title","end_char_index":12,"start_char_index":0}}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn page_location_serialization() {
        let page_location = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_page_number: 5,
            start_page_number: 3,
        };

        let delta = CitationsDelta::with_page_location(page_location);

        let json = serde_json::to_string(&delta).unwrap();
        let expected = r#"{"citation":{"type":"page_location","cited_text":"example text","document_index":0,"document_title":"Document Title","end_page_number":5,"start_page_number":3}}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn content_block_location_serialization() {
        let content_block_location = CitationContentBlockLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_block_index: 3,
            start_block_index: 1,
        };

        let delta = CitationsDelta::with_content_block_location(content_block_location);

        let json = serde_json::to_string(&delta).unwrap();
        let expected = r#"{"citation":{"type":"content_block_location","cited_text":"example text","document_index":0,"document_title":"Document Title","end_block_index":3,"start_block_index":1}}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn web_search_result_location_serialization() {
        let web_search_result_location = CitationWebSearchResultLocation {
            cited_text: "example text".to_string(),
            encrypted_index: "abc123".to_string(),
            title: Some("Example Website".to_string()),
            url: "https://example.com/page".to_string(),
        };

        let delta = CitationsDelta::with_web_search_result_location(web_search_result_location);

        let json = serde_json::to_string(&delta).unwrap();
        let expected = r#"{"citation":{"type":"web_search_result_location","cited_text":"example text","encrypted_index":"abc123","title":"Example Website","url":"https://example.com/page"}}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let char_location_json = r#"{"citation":{"type":"char_location","cited_text":"example text","document_index":0,"document_title":"Document Title","end_char_index":12,"start_char_index":0}}"#;
        let delta: CitationsDelta = serde_json::from_str(char_location_json).unwrap();

        match delta.citation {
            Citation::CharLocation(loc) => {
                assert_eq!(loc.cited_text, "example text");
                assert_eq!(loc.document_index, 0);
                assert_eq!(loc.document_title, Some("Document Title".to_string()));
                assert_eq!(loc.end_char_index, 12);
                assert_eq!(loc.start_char_index, 0);
            }
            _ => panic!("Expected CharLocation"),
        }
    }
}
