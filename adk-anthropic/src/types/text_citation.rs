use serde::{Deserialize, Serialize};

use crate::types::{
    CitationCharLocation, CitationContentBlockLocation, CitationPageLocation,
    CitationWebSearchResultLocation,
};

/// A citation reference in a TextBlock.
///
/// This enum represents the different types of citations that can be included
/// in a text block's content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum TextCitation {
    /// A character-based location citation
    #[serde(rename = "char_location")]
    CharLocation(CitationCharLocation),

    /// A page-based location citation
    #[serde(rename = "page_location")]
    PageLocation(CitationPageLocation),

    /// A content block-based location citation
    #[serde(rename = "content_block_location")]
    ContentBlockLocation(CitationContentBlockLocation),

    /// A web search result location citation
    #[serde(rename = "web_search_result_location")]
    WebSearchResultLocation(CitationWebSearchResultLocation),
}

impl TextCitation {
    /// Creates a new character-based location citation
    pub fn char_location(
        cited_text: String,
        document_index: i32,
        start_char_index: i32,
        end_char_index: i32,
        document_title: Option<String>,
    ) -> Self {
        let char_location = CitationCharLocation {
            cited_text,
            document_index,
            document_title,
            end_char_index,
            start_char_index,
        };
        Self::CharLocation(char_location)
    }

    /// Creates a new page-based location citation
    pub fn page_location(
        cited_text: String,
        document_index: i32,
        start_page_number: i32,
        end_page_number: i32,
        document_title: Option<String>,
    ) -> Self {
        let page_location = CitationPageLocation {
            cited_text,
            document_index,
            document_title,
            end_page_number,
            start_page_number,
        };
        Self::PageLocation(page_location)
    }

    /// Creates a new content block-based location citation
    pub fn content_block_location(
        cited_text: String,
        document_index: i32,
        start_block_index: i32,
        end_block_index: i32,
        document_title: Option<String>,
    ) -> Self {
        let content_block_location = CitationContentBlockLocation {
            cited_text,
            document_index,
            document_title,
            end_block_index,
            start_block_index,
        };
        Self::ContentBlockLocation(content_block_location)
    }

    /// Creates a new web search result location citation
    pub fn web_search_result_location(
        cited_text: String,
        encrypted_index: String,
        url: String,
        title: Option<String>,
    ) -> Self {
        let web_search_result_location =
            CitationWebSearchResultLocation { cited_text, encrypted_index, title, url };
        Self::WebSearchResultLocation(web_search_result_location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn char_location_serialization() {
        let char_location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let citation = TextCitation::CharLocation(char_location);

        let json = serde_json::to_value(&citation).unwrap();
        let expected = serde_json::json!({
            "type": "char_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_char_index": 12,
            "start_char_index": 0
        });

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

        let citation = TextCitation::PageLocation(page_location);

        let json = serde_json::to_value(&citation).unwrap();
        let expected = serde_json::json!({
            "type": "page_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_page_number": 5,
            "start_page_number": 3
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn content_block_location_serialization() {
        let content_block_location = CitationContentBlockLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            start_block_index: 1,
            end_block_index: 5,
        };

        let citation = TextCitation::ContentBlockLocation(content_block_location);

        let json = serde_json::to_value(&citation).unwrap();
        let expected = serde_json::json!({
            "type": "content_block_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_block_index": 5,
            "start_block_index": 1
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn web_search_result_location_serialization() {
        let web_search_location = CitationWebSearchResultLocation {
            cited_text: "example text".to_string(),
            encrypted_index: "abc123".to_string(),
            title: Some("Example Website".to_string()),
            url: "https://example.com/page".to_string(),
        };

        let citation = TextCitation::WebSearchResultLocation(web_search_location);

        let json = serde_json::to_value(&citation).unwrap();
        let expected = serde_json::json!({
            "type": "web_search_result_location",
            "cited_text": "example text",
            "encrypted_index": "abc123",
            "title": "Example Website",
            "url": "https://example.com/page"
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({
            "type": "char_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_char_index": 12,
            "start_char_index": 0
        });
        let citation: TextCitation = serde_json::from_value(json).unwrap();

        match citation {
            TextCitation::CharLocation(location) => {
                assert_eq!(location.cited_text, "example text");
                assert_eq!(location.document_index, 0);
                assert_eq!(location.document_title, Some("Document Title".to_string()));
                assert_eq!(location.end_char_index, 12);
                assert_eq!(location.start_char_index, 0);
            }
            _ => panic!("Expected CharLocation"),
        }
    }

    #[test]
    fn char_location_method() {
        let citation = TextCitation::char_location(
            "example text".to_string(),
            0,
            0,
            12,
            Some("Document Title".to_string()),
        );

        let json_value = serde_json::to_value(&citation).unwrap();
        let expected = json!({
            "type": "char_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_char_index": 12,
            "start_char_index": 0
        });

        assert_eq!(json_value, expected);
    }

    #[test]
    fn page_location_method() {
        let citation = TextCitation::page_location(
            "example text".to_string(),
            0,
            3,
            5,
            Some("Document Title".to_string()),
        );

        let json_value = serde_json::to_value(&citation).unwrap();
        let expected = json!({
            "type": "page_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_page_number": 5,
            "start_page_number": 3
        });

        assert_eq!(json_value, expected);
    }

    #[test]
    fn content_block_location_method() {
        let citation = TextCitation::content_block_location(
            "example text".to_string(),
            0,
            2,
            4,
            Some("Document Title".to_string()),
        );

        let json_value = serde_json::to_value(&citation).unwrap();
        let expected = json!({
            "type": "content_block_location",
            "cited_text": "example text",
            "document_index": 0,
            "document_title": "Document Title",
            "end_block_index": 4,
            "start_block_index": 2
        });

        assert_eq!(json_value, expected);
    }

    #[test]
    fn web_search_result_location_method() {
        let citation = TextCitation::web_search_result_location(
            "example text".to_string(),
            "encrypted123".to_string(),
            "https://example.com".to_string(),
            Some("Example Website".to_string()),
        );

        let json_value = serde_json::to_value(&citation).unwrap();
        let expected = json!({
            "type": "web_search_result_location",
            "cited_text": "example text",
            "encrypted_index": "encrypted123",
            "title": "Example Website",
            "url": "https://example.com"
        });

        assert_eq!(json_value, expected);
    }

    #[test]
    fn optional_fields_are_omitted() {
        let citation = TextCitation::char_location("example text".to_string(), 0, 0, 12, None);

        let json_value = serde_json::to_value(&citation).unwrap();
        let expected = json!({
            "type": "char_location",
            "cited_text": "example text",
            "document_index": 0,
            "end_char_index": 12,
            "start_char_index": 0
        });

        assert_eq!(json_value, expected);
    }
}
