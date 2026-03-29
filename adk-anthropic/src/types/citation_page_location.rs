use serde::{Deserialize, Serialize};

/// Represents a page-based location citation.
///
/// This type is used to indicate a specific span of pages in a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationPageLocation {
    /// The text that was cited
    pub cited_text: String,

    /// The index of the document in the input context
    pub document_index: i32,

    /// Optional title of the document
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,

    /// The end page number (inclusive) of the citation in the document
    pub end_page_number: i32,

    /// The start page number (inclusive) of the citation in the document
    pub start_page_number: i32,
}

impl CitationPageLocation {
    /// Creates a new CitationPageLocation
    pub fn new(
        cited_text: String,
        document_index: i32,
        start_page_number: i32,
        end_page_number: i32,
        document_title: Option<String>,
    ) -> Self {
        Self { cited_text, document_index, document_title, end_page_number, start_page_number }
    }

    /// Returns the number of pages in the citation span (inclusive range)
    pub fn page_count(&self) -> i32 {
        self.end_page_number - self.start_page_number + 1
    }

    /// Returns true if the citation is only for a single page
    pub fn is_single_page(&self) -> bool {
        self.start_page_number == self.end_page_number
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let location = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: Some("Document Title".to_string()),
            end_page_number: 5,
            start_page_number: 3,
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","document_index":0,"document_title":"Document Title","end_page_number":5,"start_page_number":3}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn serialization_without_title() {
        let location = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_page_number: 5,
            start_page_number: 3,
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","document_index":0,"end_page_number":5,"start_page_number":3}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"cited_text":"example text","document_index":0,"document_title":"Document Title","end_page_number":5,"start_page_number":3,"type":"page_location"}"#;
        let location: CitationPageLocation = serde_json::from_str(json).unwrap();

        assert_eq!(location.cited_text, "example text");
        assert_eq!(location.document_index, 0);
        assert_eq!(location.document_title, Some("Document Title".to_string()));
        assert_eq!(location.end_page_number, 5);
        assert_eq!(location.start_page_number, 3);
    }

    #[test]
    fn page_count() {
        let location = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_page_number: 5,
            start_page_number: 3,
        };

        assert_eq!(location.page_count(), 3);
    }

    #[test]
    fn is_single_page() {
        let single_page = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_page_number: 3,
            start_page_number: 3,
        };

        let multi_page = CitationPageLocation {
            cited_text: "example text".to_string(),
            document_index: 0,
            document_title: None,
            end_page_number: 5,
            start_page_number: 3,
        };

        assert!(single_page.is_single_page());
        assert!(!multi_page.is_single_page());
    }
}
