use serde::{Deserialize, Serialize};

/// Represents a character-based location citation.
///
/// This type is used to indicate a specific span of text in a document by
/// character indices.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationCharLocation {
    /// The text that was cited
    pub cited_text: String,

    /// The index of the document in the input context
    pub document_index: i32,

    /// Optional title of the document
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,

    /// The end character index (exclusive) of the citation in the document
    pub end_char_index: i32,

    /// The start character index (inclusive) of the citation in the document
    pub start_char_index: i32,
}

impl CitationCharLocation {
    /// Creates a new CitationCharLocation
    pub fn new(
        cited_text: String,
        document_index: i32,
        start_char_index: i32,
        end_char_index: i32,
        document_title: Option<String>,
    ) -> Self {
        Self { cited_text, document_index, document_title, end_char_index, start_char_index }
    }

    /// Returns the length of the cited text in characters
    pub fn length(&self) -> i32 {
        self.end_char_index - self.start_char_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_char_index: 12,
            start_char_index: 0,
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","document_index":0,"document_title":"Document Title","end_char_index":12,"start_char_index":0}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn serialization_without_title() {
        let location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_char_index: 12,
            start_char_index: 0,
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","document_index":0,"end_char_index":12,"start_char_index":0}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"cited_text":"example text","document_index":0,"document_title":"Document Title","end_char_index":12,"start_char_index":0,"type":"char_location"}"#;
        let location: CitationCharLocation = serde_json::from_str(json).unwrap();

        assert_eq!(location.cited_text, "example text");
        assert_eq!(location.document_index, 0);
        assert_eq!(location.document_title, Some("Document Title".to_string()));
        assert_eq!(location.end_char_index, 12);
        assert_eq!(location.start_char_index, 0);
    }

    #[test]
    fn length() {
        let location = CitationCharLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_char_index: 12,
            start_char_index: 0,
        };

        assert_eq!(location.length(), 12);
    }
}
