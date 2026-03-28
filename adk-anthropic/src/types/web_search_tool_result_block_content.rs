use serde::{Deserialize, Serialize};

use crate::types::{WebSearchResultBlock, WebSearchToolResultError};

/// Content of a web search tool result.
///
/// This can either be a list of search results or an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WebSearchToolResultBlockContent {
    /// A list of web search results.
    Results(Vec<WebSearchResultBlock>),

    /// An error that occurred during the web search.
    Error(WebSearchToolResultError),
}

impl WebSearchToolResultBlockContent {
    /// Creates a new WebSearchToolResultBlockContent with the specified results.
    pub fn with_results(results: Vec<WebSearchResultBlock>) -> Self {
        Self::Results(results)
    }

    /// Creates a new WebSearchToolResultBlockContent with the specified error.
    pub fn with_error(error: WebSearchToolResultError) -> Self {
        Self::Error(error)
    }

    /// Returns true if the content is a list of results.
    pub fn is_results(&self) -> bool {
        matches!(self, WebSearchToolResultBlockContent::Results(_))
    }

    /// Returns true if the content is an error.
    pub fn is_error(&self) -> bool {
        matches!(self, WebSearchToolResultBlockContent::Error(_))
    }

    /// Returns a reference to the results if this is a Results variant,
    /// or None otherwise.
    pub fn as_results(&self) -> Option<&Vec<WebSearchResultBlock>> {
        match self {
            WebSearchToolResultBlockContent::Results(results) => Some(results),
            _ => None,
        }
    }

    /// Returns a reference to the error if this is an Error variant,
    /// or None otherwise.
    pub fn as_error(&self) -> Option<&WebSearchToolResultError> {
        match self {
            WebSearchToolResultBlockContent::Error(error) => Some(error),
            _ => None,
        }
    }

    /// Returns the number of results if this is a Results variant,
    /// or 0 if this is an Error variant.
    pub fn result_count(&self) -> usize {
        match self {
            WebSearchToolResultBlockContent::Results(results) => results.len(),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WebSearchErrorCode;
    use serde_json::Value;

    #[test]
    fn results_serialization() {
        let results = vec![
            WebSearchResultBlock::new(
                "encrypted-data-1",
                "Example Page 1",
                "https://example.com/page1",
            )
            .with_page_age("2 days ago".to_string()),
            WebSearchResultBlock::new(
                "encrypted-data-2",
                "Example Page 2",
                "https://example.com/page2",
            ),
        ];

        let content = WebSearchToolResultBlockContent::with_results(results);

        let json = serde_json::to_string(&content).unwrap();
        let json_value: Value = serde_json::from_str(&json).unwrap();
        let expected_value: Value = serde_json::from_str(r#"[{"type":"web_search_result","encrypted_content":"encrypted-data-1","page_age":"2 days ago","title":"Example Page 1","url":"https://example.com/page1"},{"type":"web_search_result","encrypted_content":"encrypted-data-2","title":"Example Page 2","url":"https://example.com/page2"}]"#).unwrap();

        assert_eq!(json_value, expected_value);
    }

    #[test]
    fn error_serialization() {
        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let content = WebSearchToolResultBlockContent::with_error(error);

        let json = serde_json::to_string(&content).unwrap();
        let json_value: Value = serde_json::from_str(&json).unwrap();
        let expected_value: Value =
            serde_json::from_str(r#"{"error_code":"invalid_tool_input"}"#).unwrap();

        assert_eq!(json_value, expected_value);
    }

    #[test]
    fn results_deserialization() {
        let json = r#"[{"type":"web_search_result","encrypted_content":"encrypted-data-1","page_age":"2 days ago","title":"Example Page 1","url":"https://example.com/page1"},{"type":"web_search_result","encrypted_content":"encrypted-data-2","title":"Example Page 2","url":"https://example.com/page2"}]"#;
        let content: WebSearchToolResultBlockContent = serde_json::from_str(json).unwrap();

        assert!(content.is_results());
        assert!(!content.is_error());

        let results = content.as_results().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].encrypted_content, "encrypted-data-1");
        assert_eq!(results[0].title, "Example Page 1");
        assert_eq!(results[1].encrypted_content, "encrypted-data-2");
        assert_eq!(results[1].title, "Example Page 2");
    }

    #[test]
    fn error_deserialization() {
        let json = r#"{"error_code":"invalid_tool_input"}"#;
        let content: WebSearchToolResultBlockContent = serde_json::from_str(json).unwrap();

        assert!(!content.is_results());
        assert!(content.is_error());

        let error = content.as_error().unwrap();
        assert_eq!(error.error_code, WebSearchErrorCode::InvalidToolInput);
    }

    #[test]
    fn result_count() {
        let results = vec![
            WebSearchResultBlock::new(
                "encrypted-data-1",
                "Example Page 1",
                "https://example.com/page1",
            )
            .with_page_age("2 days ago".to_string()),
            WebSearchResultBlock::new(
                "encrypted-data-2",
                "Example Page 2",
                "https://example.com/page2",
            ),
        ];

        let content = WebSearchToolResultBlockContent::with_results(results);
        assert_eq!(content.result_count(), 2);

        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let content = WebSearchToolResultBlockContent::with_error(error);
        assert_eq!(content.result_count(), 0);
    }
}
