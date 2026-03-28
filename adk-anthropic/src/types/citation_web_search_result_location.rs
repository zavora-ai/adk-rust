use serde::{Deserialize, Serialize};

/// Represents a web search result location citation.
///
/// This type is used to specify a citation that references web search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitationWebSearchResultLocation {
    /// The text that was cited
    pub cited_text: String,

    /// An encrypted identifier for the web search result
    pub encrypted_index: String,

    /// Optional title of the web page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// The URL of the web page containing the cited content
    pub url: String,
}

impl CitationWebSearchResultLocation {
    /// Creates a new CitationWebSearchResultLocation
    pub fn new(
        cited_text: String,
        encrypted_index: String,
        url: String,
        title: Option<String>,
    ) -> Self {
        Self { cited_text, encrypted_index, title, url }
    }

    /// Returns the URL domain (host) part of the citation
    pub fn domain(&self) -> Option<String> {
        url::Url::parse(&self.url).ok().and_then(|url| url.host_str().map(|s| s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let location = CitationWebSearchResultLocation {
            cited_text: "example text".to_string(),
            encrypted_index: "abc123".to_string(),
            title: Some("Example Website".to_string()),
            url: "https://example.com/page".to_string(),
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","encrypted_index":"abc123","title":"Example Website","url":"https://example.com/page"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn serialization_without_title() {
        let location = CitationWebSearchResultLocation {
            cited_text: "example text".to_string(),
            encrypted_index: "abc123".to_string(),
            title: None,
            url: "https://example.com/page".to_string(),
        };

        let json = serde_json::to_string(&location).unwrap();
        let expected = r#"{"cited_text":"example text","encrypted_index":"abc123","url":"https://example.com/page"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"cited_text":"example text","encrypted_index":"abc123","title":"Example Website","type":"web_search_result_location","url":"https://example.com/page"}"#;
        let location: CitationWebSearchResultLocation = serde_json::from_str(json).unwrap();

        assert_eq!(location.cited_text, "example text");
        assert_eq!(location.encrypted_index, "abc123");
        assert_eq!(location.title, Some("Example Website".to_string()));
        assert_eq!(location.url, "https://example.com/page");
    }

    #[test]
    fn domain() {
        let location = CitationWebSearchResultLocation {
            cited_text: "example text".to_string(),
            encrypted_index: "abc123".to_string(),
            title: None,
            url: "https://example.com/page".to_string(),
        };

        assert_eq!(location.domain(), Some("example.com".to_string()));
    }
}
